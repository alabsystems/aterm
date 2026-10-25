// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Aos M4 evaluator.
//!
//! # The locks
//!
//! A result cannot be written by hand:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m4::Admitted;
//! let _ = Admitted {
//!     policy: todo!(),
//!     plan: todo!(),
//!     bundle: todo!(),
//!     useful: true,
//!     gain: todo!(),
//! };
//! ```
//!
//! nor copied:
//!
//! ```compile_fail,E0277
//! use aterm_effects::aos_momentum::m4::Admitted;
//! fn copy(a: &Admitted) -> Admitted { Clone::clone(a) }
//! ```
//!
//! nor a policy:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::policy::AcceptancePolicy;
//! let _ = AcceptancePolicy { id: todo!(), terms: todo!() };
//! ```
//!
//! nor the feedback a proposer is shown:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::search::TrainFeedback;
//! let _ = TrainFeedback { plan: todo!(), admitted: true, law: None, ticks: Some(1) };
//! ```
//!
//! ```compile_fail,E0624
//! use aterm_aos::Digest;
//! use aterm_effects::aos_momentum::search::TrainFeedback;
//! let _ = TrainFeedback::admitted(Digest([0; 32]), 1);
//! ```
//!
//! nor a planned code, which only the evaluator and publication build:
//!
//! ```compile_fail,E0624
//! use aterm_effects::aos_momentum::PrebuiltMomentum;
//! use aterm_effects::aos_momentum::plan::SHIPPED_PLAN;
//! let _ = PrebuiltMomentum::planned(SHIPPED_PLAN, "build");
//! ```
//!
//! nor a selection or a verified bundle:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m4::Selection;
//! let _ = Selection {
//!     build: todo!(),
//!     winner: todo!(),
//!     proposer: todo!(),
//!     round: 0,
//!     index: 0,
//!     stats: todo!(),
//!     mode: todo!(),
//! };
//! ```
//!
//! ```compile_fail,E0423
//! use aterm_effects::aos_momentum::bundle::CandidateBundle;
//! let _ = CandidateBundle(todo!());
//! ```
//!
//! and the held-out split cannot be looked at twice for one selection:
//!
//! ```compile_fail,E0382
//! use aterm_effects::aos_momentum::m4::Evaluator;
//! use aterm_effects::aos_momentum::search::GridProposer;
//! fn twice(ev: &Evaluator) {
//!     let Ok(s) = ev.select(&mut GridProposer::new()) else { return };
//!     let _ = ev.held_out(s);
//!     let _ = ev.held_out(s);
//! }
//! ```
//!
//! # The M6 locks (design §13.3)
//!
//! A training label cannot be written by hand, built, or read for anything but training facts:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m4::TrainLabel;
//! let _ = TrainLabel { id: todo!(), plan: todo!(), outcome: todo!() };
//! ```
//!
//! ```compile_fail,E0624
//! use aterm_effects::aos_momentum::m4::TrainLabel;
//! let _ = TrainLabel::of(todo!(), todo!());
//! ```
//!
//! ```compile_fail,E0599
//! use aterm_effects::aos_momentum::m4::TrainLabel;
//! fn peek(l: &TrainLabel) -> Option<u64> { l.held_out_ticks() }
//! ```
//!
//! ```compile_fail,E0616
//! use aterm_effects::aos_momentum::m4::TrainLabel;
//! fn peek(l: &TrainLabel) { let _ = &l.outcome; }
//! ```
//!
//! A labelling token cannot be written by hand, copied, or used twice:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m4::LookToken;
//! let _ = LookToken { bound: todo!() };
//! ```
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m4::ReadToken;
//! let _ = ReadToken { bound: todo!() };
//! ```
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m4::ReplayToken;
//! let _ = ReplayToken { bound: todo!() };
//! ```
//!
//! ```compile_fail,E0277
//! use aterm_effects::aos_momentum::m4::LookToken;
//! fn copy(t: &LookToken) -> LookToken { Clone::clone(t) }
//! ```
//!
//! ```compile_fail,E0277
//! use aterm_effects::aos_momentum::m4::ReadToken;
//! fn copy(t: &ReadToken) -> ReadToken { Clone::clone(t) }
//! ```
//!
//! ```compile_fail,E0277
//! use aterm_effects::aos_momentum::m4::ReplayToken;
//! fn copy(t: &ReplayToken) -> ReplayToken { Clone::clone(t) }
//! ```
//!
//! ```compile_fail,E0382
//! use aterm_effects::aos_momentum::m4::{Evaluator, LabelScope, LookToken, Serial};
//! fn look_twice(ev: &Evaluator, t: LookToken, plans: &[[u8; 12]]) {
//!     let _ = ev.label_spread(LabelScope::HeldOut(t), plans, &Serial);
//!     let _ = ev.label_spread(LabelScope::HeldOut(t), plans, &Serial);
//! }
//! ```
//!
//! ```compile_fail,E0382
//! use aterm_effects::aos_momentum::m4::{Evaluator, LabelScope, ReadToken, Serial};
//! fn read_twice(ev: &Evaluator, t: ReadToken, plans: &[[u8; 12]]) {
//!     let _ = ev.label_spread(LabelScope::Report(t), plans, &Serial);
//!     let _ = ev.label_spread(LabelScope::Report(t), plans, &Serial);
//! }
//! ```
//!
//! ```compile_fail,E0382
//! use aterm_effects::aos_momentum::m4::{Evaluator, LabelScope, ReplayToken, Serial};
//! fn replay_twice(ev: &Evaluator, t: ReplayToken, plans: &[[u8; 12]]) {
//!     let _ = ev.label_spread(LabelScope::Replay(t), plans, &Serial);
//!     let _ = ev.label_spread(LabelScope::Replay(t), plans, &Serial);
//! }
//! ```
//!
//! A pool's outcome is no selection, so the held-out gate does not take one:
//!
//! ```compile_fail,E0308
//! use aterm_effects::aos_momentum::m4::{Evaluator, PoolOutcome};
//! fn gate(ev: &Evaluator, o: PoolOutcome) { let _ = ev.held_out(o); }
//! ```

use core::convert::Infallible;
use core::fmt;
use core::ops::ControlFlow;
use core::sync::atomic::{AtomicU64, Ordering};
use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::time::Duration;

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::NoEffects;
use aterm_aos::evidence::TESTED_ASSUMPTIONS;
use aterm_aos::replay::replay;
use aterm_aos::task::{Code, Schema, state_digest};
use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, Digest};
use aterm_time::Instant;

use super::bundle::{
    BUNDLE_GRADE, BundleField, CandidateBundle, MAX_TIMINGS, PLAN_ASSUMPTIONS, Row, RowVerdict,
    SelectionPart, Timing, UNDECLARED, UncheckedBundle, add,
};
use super::corpus::{Corpus, CorpusSplit, ItemKind};
use super::dist::{Claim, ClaimKind};
use super::fidelity::{LANE_SETTLES, LaneFidelity, ORACLE};
use super::lane::{LANE_MODEL, Scenario, encode_glass, run};
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
use super::m6_policy::{
    JUDGED, LOOK_LIST_CAP, MAX_JUDGED, PoolRefusal, READ_LIST_CAP, SetId, SetTag, Side, set_pools,
    sets_v1, side,
};
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
use super::plan::PLAN_LEN;
use super::plan::{PLAN_INTERPRETER, PlanRefusal, SchedulePlan};
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
use super::policy::GRID_V1;
use super::policy::{
    AcceptancePolicy, Body, Budgets, CORPUS_FOLD, ItemFacts, PolicyRefusal, Totals,
};
use super::search::{
    GridProposer, Mode, Proposer, ProposerId, SearchStats, TrainFeedback, candidate_id,
};
use super::{
    HostMode, MOMENTUM_MAX_EVENTS, MOMENTUM_QUEUE_CAP, MomentumCode, MomentumHost,
    MomentumObservation, MomentumTrace, NoStrandedLight, PrebuiltMomentum, Sched, profile_artifact,
    schema_artifact,
};
use crate::cursor_momentum::MomentumGlowFrame;
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
use core::num::NonZeroU64;
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
use std::collections::BTreeMap;
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
use std::sync::{Mutex, PoisonError};

/// The name of [`CandidateRefusal::Budget`].
pub const BUDGET: &str = "Budget";

/// The name of [`CandidateRefusal::Decode`].
pub const PLAN_REFUSED: &str = "PlanRefusal";

/// The law of a lane item whose host did not log every call of its run in one log.
pub const LANE_NOT_LOGGED: &str = "LaneNotLogged";

/// Law failure for a lane item whose replayed log diverges from the live state and observations.
pub const LANE_NOT_REPRODUCED: &str = "LaneNotReproduced";

/// The entry law of a golden trace that does not decode, read or restore in this build.
pub const GOLDEN_UNREADABLE: &str = "GoldenTraceUnreadable";

const UNREPLAYED: Digest = Digest([0; 32]);

/// The held-out corpus items judged in this process, by any evaluator (design §13.3).
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
static HELD_OUT_JUDGED: AtomicU64 = AtomicU64::new(0);

/// How many held-out corpus items any evaluator of this process has judged; every M6 lab command
/// requires it to read 0 (design §13.3).
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[must_use]
pub fn held_out_judged() -> u64 {
    HELD_OUT_JUDGED.load(Ordering::SeqCst)
}

#[inline]
fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Why a candidate is not admitted: the first step of the admission rule it failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateRefusal {
    /// The proposal is over the byte budget; it was neither digested nor decoded.
    Budget {
        /// Its length.
        len: usize,
        /// [`MAX_PROPOSAL_BYTES`](super::policy::MAX_PROPOSAL_BYTES).
        limit: usize,
    },
    /// The bytes are not a plan.
    Decode(PlanRefusal),
    /// The planned code does not enter on the golden trace's checkpoint, or the trace does not read
    /// in this build ([`GOLDEN_UNREADABLE`]).
    Entry {
        /// The item.
        item: String,
        /// The entry law.
        law: &'static str,
        /// The offending values.
        detail: String,
    },
    /// The golden trace, replayed open loop with the planned code, fails `NoStrandedLight`.
    OpenLoop {
        /// The item.
        item: String,
        /// The law that failed first.
        law: &'static str,
        /// The input cursor it failed at.
        cursor: u64,
        /// The offending values.
        detail: String,
    },
    /// The host did not log every call of the run in one log.
    LaneNotLogged {
        /// The item.
        item: String,
    },
    /// Host log replayed from its checkpoint diverges from the live state and observation chain.
    LaneNotReproduced {
        /// The item.
        item: String,
    },
    /// The host's log, replayed, fails `NoStrandedLight`.
    StrandedLight {
        /// The item.
        item: String,
        /// The law that failed first.
        law: &'static str,
        /// The input cursor it failed at.
        cursor: u64,
        /// The offending values.
        detail: String,
    },
    /// The lane run fails lane fidelity.
    LaneFidelity {
        /// The item.
        item: String,
        /// The law that failed first.
        law: &'static str,
        /// The tick ordinal it failed at.
        cursor: u64,
        /// The offending values.
        detail: String,
    },
}

impl CandidateRefusal {
    /// The refusal's law, as a proposer's feedback names it: the failing law.
    #[must_use]
    pub const fn law(&self) -> &'static str {
        match self {
            CandidateRefusal::Budget { .. } => BUDGET,
            CandidateRefusal::Decode(_) => PLAN_REFUSED,
            CandidateRefusal::Entry { law, .. }
            | CandidateRefusal::OpenLoop { law, .. }
            | CandidateRefusal::StrandedLight { law, .. }
            | CandidateRefusal::LaneFidelity { law, .. } => law,
            CandidateRefusal::LaneNotLogged { .. } => LANE_NOT_LOGGED,
            CandidateRefusal::LaneNotReproduced { .. } => LANE_NOT_REPRODUCED,
        }
    }

    /// The item it was refused on, if it got as far as one.
    #[must_use]
    pub fn item(&self) -> Option<&str> {
        match self {
            CandidateRefusal::Budget { .. } | CandidateRefusal::Decode(_) => None,
            CandidateRefusal::Entry { item, .. }
            | CandidateRefusal::OpenLoop { item, .. }
            | CandidateRefusal::LaneNotLogged { item }
            | CandidateRefusal::LaneNotReproduced { item }
            | CandidateRefusal::StrandedLight { item, .. }
            | CandidateRefusal::LaneFidelity { item, .. } => Some(item),
        }
    }
}

impl fmt::Display for CandidateRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateRefusal::Budget { len, limit } => write!(
                f,
                "candidate refused: the proposal is {len} bytes, more than the budget's {limit}"
            ),
            CandidateRefusal::Decode(r) => write!(f, "candidate refused: {r}"),
            CandidateRefusal::Entry { item, law, detail } => write!(
                f,
                "candidate refused: it does not enter on the checkpoint of {item} ({law}): {detail}"
            ),
            CandidateRefusal::OpenLoop {
                item,
                law,
                cursor,
                detail,
            } => write!(
                f,
                "candidate refused: {item}, replayed open loop with it, fails {law} at cursor {cursor}: {detail}"
            ),
            CandidateRefusal::LaneNotLogged { item } => write!(
                f,
                "candidate refused: the host on {item} did not log every call of the run"
            ),
            CandidateRefusal::LaneNotReproduced { item } => write!(
                f,
                "candidate refused: the log of {item}, replayed from its checkpoint, does not reproduce the live head"
            ),
            CandidateRefusal::StrandedLight {
                item,
                law,
                cursor,
                detail,
            } => write!(
                f,
                "candidate refused: the replayed log of {item} fails {law} at cursor {cursor}: {detail}"
            ),
            CandidateRefusal::LaneFidelity {
                item,
                law,
                cursor,
                detail,
            } => write!(
                f,
                "candidate refused: the lane run of {item} fails {law} at tick {cursor}: {detail}"
            ),
        }
    }
}

impl std::error::Error for CandidateRefusal {}

/// Why a search has no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoOutcome {
    /// No candidate was admitted on every training item.
    NoneAdmitted {
        /// What the search counted.
        stats: SearchStats,
    },
    /// The training winner does not beat the baseline's training `ΣT` and `ΣZ`.
    NoGain {
        /// The training winner.
        plan: SchedulePlan,
        /// Its training sums against the baseline's.
        train: Compared,
        /// What the search counted.
        stats: SearchStats,
    },
    /// The training winner is refused on a held-out item; no runner-up is tried.
    HeldOut {
        /// The training winner.
        plan: SchedulePlan,
        /// The refusal.
        refusal: CandidateRefusal,
    },
    /// The selection was made by an evaluator of another build.
    Foreign {
        /// The build it was made in.
        build: String,
    },
}

impl fmt::Display for NoOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoOutcome::NoneAdmitted { stats } => write!(
                f,
                "no outcome: none of {} distinct candidates was admitted on every training item",
                stats.distinct
            ),
            NoOutcome::NoGain { plan, train, .. } => write!(
                f,
                "no outcome: the training winner {plan:?} (T {} Z {}) does not beat the baseline (T {} Z {})",
                train.candidate.ticks,
                train.candidate.wasted,
                train.baseline.ticks,
                train.baseline.wasted
            ),
            NoOutcome::HeldOut { plan, refusal } => {
                write!(
                    f,
                    "no outcome: the training winner {plan:?} on the held-out split: {refusal}"
                )
            }
            NoOutcome::Foreign { build } => write!(
                f,
                "no outcome: the selection was made in the build {build}, not this evaluator's"
            ),
        }
    }
}

impl std::error::Error for NoOutcome {}

/// A candidate's sums against the baseline's, over one split or one item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compared {
    /// The candidate's `ΣT`, `ΣP`, `ΣZ`.
    pub candidate: Totals,
    /// The baseline's.
    pub baseline: Totals,
}

impl Compared {
    /// The candidate's `ΣT` and `ΣZ` are both below the baseline's.
    #[inline]
    #[must_use]
    pub const fn beats(&self) -> bool {
        self.candidate.ticks < self.baseline.ticks && self.candidate.wasted < self.baseline.wasted
    }

    /// `1 − ΣT_c / ΣT_B` in parts per million, truncated toward zero (0 when `ΣT_B` is 0).
    #[must_use]
    pub fn gain_ppm(&self) -> i64 {
        let (c, b) = (
            i128::from(self.candidate.ticks),
            i128::from(self.baseline.ticks),
        );
        if b == 0 {
            return 0;
        }
        i64::try_from((b - c) * 1_000_000 / b).unwrap_or(i64::MIN)
    }
}

/// A result's gain: the held-out split's sums, and `S0`'s alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gain {
    /// Over the held-out split.
    pub held_out: Compared,
    /// `S0` alone.
    pub s0: Compared,
}

impl Gain {
    /// The gain over the held-out split, in parts per million.
    #[inline]
    #[must_use]
    pub fn ppm(&self) -> i64 {
        self.held_out.gain_ppm()
    }

    /// The gain on `S0` alone, in parts per million.
    #[inline]
    #[must_use]
    pub fn s0_ppm(&self) -> i64 {
        self.s0.gain_ppm()
    }
}

/// The sums a bundle's selection states, from its rows and the policy's pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Sums {
    /// The training split.
    pub(super) train: Compared,
    /// The held-out split.
    pub(super) held_out: Compared,
    /// `S0` alone.
    pub(super) s0: Compared,
}

/// `rows`' sums against `policy`'s baseline.
pub(super) fn sums(policy: &AcceptancePolicy, rows: &[Row]) -> Sums {
    let sum = |split| {
        rows.iter()
            .filter(|r| r.split == split)
            .fold(Totals::default(), |t, r| add(t, r.totals()))
    };
    let story = rows
        .iter()
        .find(|r| r.kind == ItemKind::M3Story)
        .map(Row::totals)
        .unwrap_or_default();
    let pinned = policy
        .items()
        .iter()
        .find(|p| p.kind() == ItemKind::M3Story)
        .map(|p| {
            let b = p.baseline();
            Totals {
                ticks: b.ticks,
                presented: b.presented,
                wasted: b.wasted,
            }
        })
        .unwrap_or_default();
    Sums {
        train: Compared {
            candidate: sum(CorpusSplit::Train),
            baseline: policy.totals(CorpusSplit::Train),
        },
        held_out: Compared {
            candidate: sum(CorpusSplit::HeldOut),
            baseline: policy.totals(CorpusSplit::HeldOut),
        },
        s0: Compared {
            candidate: story,
            baseline: pinned,
        },
    }
}

/// A candidate every training row passed: built only here.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
struct TrainAdmitted {
    plan: SchedulePlan,
    /// Its training rows, with their corpus indices.
    rows: Vec<(usize, Row)>,
    /// Their `ΣT`, `ΣP`, `ΣZ`.
    totals: Totals,
}

impl TrainAdmitted {
    /// `plan` admitted with `rows`, its totals summed here.
    fn of(plan: SchedulePlan, rows: Vec<(usize, Row)>) -> TrainAdmitted {
        let totals = rows
            .iter()
            .fold(Totals::default(), |t, (_, r)| add(t, r.totals()));
        TrainAdmitted { plan, rows, totals }
    }
}

/// The selection rule's winner so far, shared by [`Fold`] and the M6 pool outcomes so both pick
/// and judge one way (design §13.3): the lowest `ΣT`, then `ΣZ`, then the plan's bytes, the first
/// of equal keys kept.
struct Winner<T> {
    best: Option<(Totals, SchedulePlan, T)>,
}

impl<T> Winner<T> {
    const fn new() -> Winner<T> {
        Winner { best: None }
    }

    /// Offers an admitted candidate's training totals and plan, with what to return for it.
    fn offer(&mut self, totals: Totals, plan: SchedulePlan, with: T) {
        let key = |t: &Totals, p: &SchedulePlan| (t.ticks, t.wasted, p.encode());
        if self
            .best
            .as_ref()
            .is_none_or(|(t, p, _)| key(&totals, &plan) < key(t, p))
        {
            self.best = Some((totals, plan, with));
        }
    }

    /// The winner, with its training sums against `policy`'s baseline: a selection iff they
    /// [`Compared::beats`].
    fn end(self, policy: &AcceptancePolicy) -> Option<(T, Compared)> {
        self.best.map(|(candidate, _, with)| {
            (
                with,
                Compared {
                    candidate,
                    baseline: policy.totals(CorpusSplit::Train),
                },
            )
        })
    }
}

/// Runs a round's jobs on any threads: a job it skips is run in place by its caller, so a spread
/// changes only how fast a search runs (design §11.4).
pub trait Spread: Sync {
    /// Runs any of `job(0)` .. `job(n - 1)`, each at most once and in any order, and returns
    /// when they are done.
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync));
}

/// Every job in order, on the caller's thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct Serial;

impl Spread for Serial {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        (0..n).for_each(job);
    }
}

/// A candidate's training rows ([`Evaluator::training_rows`]).
#[derive(Debug)]
pub(super) struct TrainingRows<B> {
    /// The rows judged, with their corpus indices, in corpus order.
    pub(super) rows: Vec<(usize, Row)>,
    /// How the run ended.
    pub(super) end: TrainingEnd<B>,
}

/// How a training run ended.
#[derive(Debug)]
pub(super) enum TrainingEnd<B> {
    /// Every training item passed.
    Admitted,
    /// The last row's item refused the candidate.
    Refused(CandidateRefusal),
    /// `keep` stopped the run after the last row.
    Stopped(B),
}

/// What a search learned of one distinct candidate.
enum Judged {
    /// Judged here on every training item, each passing.
    Admitted(TrainAdmitted),
    /// Refused here, on the training split or by its decode.
    Refused(CandidateRefusal),
    /// Claimed admitted by a worker, with its claimed training `ΣT`, and shown here to have a
    /// higher `ΣT` than the winner's (design §11.3).
    Dominated {
        /// The claimed `ΣT`.
        ticks: u64,
    },
}

/// One distinct candidate of a round.
#[derive(Clone, Copy, Debug)]
struct Distinct<'p> {
    /// Its index among the round's proposals.
    index: usize,
    /// Its id ([`candidate_id`]).
    cid: Digest,
    /// Its bytes.
    bytes: &'p [u8],
}

/// The selection every search shares: the budgets, dedupe, the counts, feedback in proposal order
/// and the key. Only this builds a [`Selection`] from a search.
struct Fold {
    id: ProposerId,
    /// Whether every round so far proposed what the grid proposes.
    is_grid: bool,
    stats: SearchStats,
    seen: BTreeSet<Digest>,
    feedback: Vec<TrainFeedback>,
    best: Winner<(TrainAdmitted, u8, u64)>,
}

impl Fold {
    fn new(id: ProposerId) -> Fold {
        Fold {
            is_grid: id == GridProposer::new().id(),
            id,
            stats: SearchStats::default(),
            seen: BTreeSet::new(),
            feedback: Vec::new(),
            best: Winner::new(),
        }
    }

    /// Round `round`'s proposals, the proposer shown the feedback so far.
    fn propose(&mut self, proposer: &mut dyn Proposer, round: u8) -> Vec<Vec<u8>> {
        self.stats.rounds = round + 1;
        let proposed = proposer.propose(round, &self.feedback);
        self.is_grid =
            self.is_grid && proposed == GridProposer::new().propose(round, &self.feedback);
        proposed
    }

    /// `proposed` counted against `budgets`: the distinct candidates, in proposal order.
    fn distinct<'p>(&mut self, budgets: &Budgets, proposed: &'p [Vec<u8>]) -> Vec<Distinct<'p>> {
        let mut out = Vec::new();
        for (index, bytes) in proposed.iter().enumerate() {
            self.stats.proposed += 1;
            if bytes.len() > budgets.proposal_bytes {
                self.stats.oversized += 1;
                continue;
            }
            let cid = candidate_id(bytes);
            if self.seen.contains(&cid) {
                self.stats.duplicates += 1;
                continue;
            }
            if self.seen.len() >= budgets.distinct {
                self.stats.over_budget += 1;
                continue;
            }
            self.seen.insert(cid);
            self.stats.distinct += 1;
            out.push(Distinct { index, cid, bytes });
        }
        out
    }

    /// One distinct candidate's judgment, taken in proposal order.
    fn take(&mut self, round: u8, index: usize, cid: Digest, judged: Judged) {
        match judged {
            Judged::Admitted(t) => {
                self.stats.admitted += 1;
                self.feedback
                    .push(TrainFeedback::admitted(cid, t.totals.ticks));
                let (totals, plan) = (t.totals, t.plan);
                self.best.offer(totals, plan, (t, round, wide(index)));
            }
            Judged::Dominated { ticks } => {
                self.stats.admitted += 1;
                self.feedback.push(TrainFeedback::admitted(cid, ticks));
            }
            Judged::Refused(r) => self.feedback.push(TrainFeedback::refused(cid, r.law())),
        }
    }

    /// The selection recorded as `mode`, or why there is none; and whether every round proposed
    /// what the grid proposes.
    fn end(self, ev: &Evaluator, mode: Mode) -> (Result<Selection, NoOutcome>, bool) {
        let Fold {
            id,
            is_grid,
            stats,
            best,
            ..
        } = self;
        let Some(((winner, round, index), train)) = best.end(&ev.policy) else {
            return (Err(NoOutcome::NoneAdmitted { stats }), is_grid);
        };
        if !train.beats() {
            let no = NoOutcome::NoGain {
                plan: winner.plan,
                train,
                stats,
            };
            return (Err(no), is_grid);
        }
        let selection = Selection {
            build: ev.build.clone(),
            winner,
            proposer: id,
            round,
            index,
            stats,
            mode,
        };
        (Ok(selection), is_grid)
    }
}

/// `f` of each of `items` through `spread`, any it skipped run in place, in order.
fn spread_map<I: Sync, T: Send + Sync>(
    spread: &dyn Spread,
    items: &[I],
    f: &(dyn Fn(&I) -> T + Sync),
) -> Vec<T> {
    let slots: Vec<OnceLock<T>> = items.iter().map(|_| OnceLock::new()).collect();
    spread.run(items.len(), &|i| {
        if let (Some(slot), Some(item)) = (slots.get(i), items.get(i)) {
            let _ = slot.get_or_init(|| f(item));
        }
    });
    slots
        .into_iter()
        .zip(items)
        .map(|(slot, item)| slot.into_inner().unwrap_or_else(|| f(item)))
        .collect()
}

#[inline]
fn index(slot: u32) -> usize {
    usize::try_from(slot).unwrap_or(usize::MAX)
}

/// How far a distributed search judges the workers' claims again (design §11.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckLevel {
    /// Every refused claim on its prefix, the best admitted claim in full, and every other
    /// admitted claim until its judged rows' `ΣT` exceed the winner's: [`Mode::Distributed`].
    Dominance,
    /// Every claim in full: [`Mode::Search`].
    All,
}

/// One job of a distributed search: a distinct round-0 candidate that decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    /// Its ordinal among the jobs.
    pub slot: u32,
    /// The plan.
    pub plan: SchedulePlan,
}

/// Why a distributed search has no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DistNoOutcome {
    /// The local fold's own: [`NoOutcome::NoneAdmitted`] or [`NoOutcome::NoGain`].
    Local(NoOutcome),
    /// A job had no claim when the search was finished.
    Unclaimed {
        /// The job.
        slot: u32,
    },
    /// A claim for a slot that is no job, or a second one.
    Misplaced {
        /// The slot.
        slot: u32,
    },
    /// A claimed row, or the claim's kind, is not what this evaluator judges.
    Contradicted {
        /// The job.
        slot: u32,
        /// The training item.
        item: String,
        /// The first part that differs.
        field: BundleField,
    },
    /// The proposer proposed after round 0, having been shown feedback that rests on claims.
    Adaptive {
        /// The round.
        round: u8,
    },
}

impl fmt::Display for DistNoOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DistNoOutcome::Local(no) => no.fmt(f),
            DistNoOutcome::Unclaimed { slot } => {
                write!(f, "void: job {slot} has no claim")
            }
            DistNoOutcome::Misplaced { slot } => {
                write!(f, "void: a claim for slot {slot}, which is no open job")
            }
            DistNoOutcome::Contradicted { slot, item, field } => write!(
                f,
                "void: job {slot}'s claim is contradicted on {item}: its {} is not what this evaluator judges",
                field.name()
            ),
            DistNoOutcome::Adaptive { round } => write!(
                f,
                "void: the proposer proposed in round {round}; only feedback-blind proposers distribute"
            ),
        }
    }
}

impl std::error::Error for DistNoOutcome {}

/// Step 5's verdict per slot, or its first contradiction; and the (slot, ordinal) pairs judged.
type Checked = (Result<Vec<Judged>, Contradiction>, Vec<(u32, usize)>);

/// A contradiction step 5 found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Contradiction {
    slot: u32,
    ordinal: usize,
    item: String,
    field: BundleField,
}

impl Contradiction {
    /// A slot whose judgments do not add up to its claim (never, for a shaped claim).
    fn unjudged(slot: u32) -> Contradiction {
        Contradiction {
            slot,
            ordinal: usize::MAX,
            item: String::new(),
            field: BundleField::Row,
        }
    }
}

/// The item of training ordinal `t`, by id.
fn claim_item(ev: &Evaluator, train: &[usize], t: usize) -> String {
    train
        .get(t)
        .and_then(|&k| ev.corpus.items().get(k))
        .map(|i| i.id().to_owned())
        .unwrap_or_default()
}

/// One task of step 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    /// A refused claim's prefix, in order.
    Prefix(u32),
    /// One training item of an admitted claim judged in full.
    Item(u32, usize),
    /// An admitted claim judged in descending claimed `T` until dominated.
    Dominate(u32),
}

/// What a task found.
#[derive(Debug)]
enum Done {
    /// The prefix refuses where it claims to.
    Refused(CandidateRefusal),
    /// The item at this corpus index passes, as claimed.
    Row(usize, Row),
    /// Its judged rows' `ΣT` exceed the winner's.
    Dominated,
    /// Every row judged, each as claimed, never exceeding the winner's `ΣT`.
    Rows(Vec<(usize, Row)>),
}

/// A task's result and the (slot, ordinal) pairs it judged.
#[derive(Debug)]
struct TaskOut {
    done: Result<Done, Contradiction>,
    judged: Vec<(u32, usize)>,
}

/// The contradiction of the lowest slot, then ordinal.
fn first_contradiction(done: &[(Task, Result<Done, Contradiction>)]) -> Option<Contradiction> {
    done.iter()
        .filter_map(|(_, d)| d.as_ref().err())
        .min_by_key(|c| (c.slot, c.ordinal))
        .cloned()
}

/// A distributed search: its round-0 jobs and the claims workers returned for them.
pub struct Distributed<'a> {
    ev: &'a Evaluator,
    proposer: &'a mut dyn Proposer,
    check: CheckLevel,
    fold: Fold,
    rounds: u8,
    /// Round 0's distinct candidates in proposal order: index, id, and the job slot or the
    /// candidate's decode refusal.
    distinct: Vec<(usize, Digest, Result<u32, CandidateRefusal>)>,
    jobs: Vec<Job>,
    claims: Vec<Option<Claim>>,
}

impl fmt::Debug for Distributed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Distributed")
            .field("check", &self.check)
            .field("jobs", &self.jobs.len())
            .field(
                "claimed",
                &self.claims.iter().filter(|c| c.is_some()).count(),
            )
            .finish_non_exhaustive()
    }
}

impl Distributed<'_> {
    /// The jobs, by slot.
    #[inline]
    #[must_use]
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    /// The claims recorded, by slot (tests only).
    #[cfg(test)]
    pub(super) fn claims(&self) -> &[Option<Claim>] {
        &self.claims
    }

    /// The check level.
    #[inline]
    #[must_use]
    pub const fn check(&self) -> CheckLevel {
        self.check
    }

    /// Records `claim` for job `slot`.
    /// # Errors
    /// [`DistNoOutcome::Misplaced`] for a slot that is no job, or one already claimed.
    pub fn claim(&mut self, slot: u32, claim: Claim) -> Result<(), DistNoOutcome> {
        match self.claims.get_mut(index(slot)) {
            Some(at @ None) => {
                *at = Some(claim);
                Ok(())
            }
            _ => Err(DistNoOutcome::Misplaced { slot }),
        }
    }

    /// The selection, every claim judged as far as design §11.3's step 5 requires before any
    /// held-out read; then the later rounds asked for nothing.
    /// # Errors
    /// The first contradiction, an unclaimed job, a proposal after round 0, or the local fold's
    /// [`NoOutcome`].
    pub fn finish(self, spread: &dyn Spread) -> Result<Selection, DistNoOutcome> {
        self.finish_noting(spread).0
    }

    /// [`Self::finish`], and the (slot, training ordinal) pairs it judged itself: indices, never a
    /// claimed row, so a caller can count local judgments against claimed ones.
    pub fn finish_noting(
        self,
        spread: &dyn Spread,
    ) -> (Result<Selection, DistNoOutcome>, Vec<(u32, usize)>) {
        let Distributed {
            ev,
            proposer,
            check,
            mut fold,
            rounds,
            distinct,
            jobs,
            claims,
        } = self;
        let mut taken = Vec::with_capacity(claims.len());
        for (job, claim) in jobs.iter().zip(claims) {
            match claim {
                Some(c) => taken.push(c),
                None => return (Err(DistNoOutcome::Unclaimed { slot: job.slot }), Vec::new()),
            }
        }
        let (verdicts, judged) = ev.check_claims(&jobs, &taken, check, spread);
        let mut verdicts: Vec<Option<Judged>> = match verdicts {
            Ok(v) => v.into_iter().map(Some).collect(),
            Err(c) => {
                let no = DistNoOutcome::Contradicted {
                    slot: c.slot,
                    item: c.item,
                    field: c.field,
                };
                return (Err(no), judged);
            }
        };
        for (at, cid, job) in distinct {
            let verdict = match job {
                Ok(slot) => match verdicts.get_mut(index(slot)).and_then(Option::take) {
                    Some(v) => v,
                    None => return (Err(DistNoOutcome::Unclaimed { slot }), judged),
                },
                Err(r) => Judged::Refused(r),
            };
            fold.take(0, at, cid, verdict);
        }
        for round in 1..rounds {
            if !fold.propose(proposer, round).is_empty() {
                return (Err(DistNoOutcome::Adaptive { round }), judged);
            }
        }
        let mode = match check {
            CheckLevel::Dominance => Mode::Distributed,
            CheckLevel::All => Mode::Search,
        };
        let (selected, is_grid) = fold.end(ev, mode);
        if is_grid && mode == Mode::Search {
            let _ = ev.grid.set(selected.as_ref().ok().map(GridSelection::of));
        }
        (selected.map_err(DistNoOutcome::Local), judged)
    }
}

/// The training winner, before the held-out split is read.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub struct Selection {
    build: String,
    winner: TrainAdmitted,
    proposer: ProposerId,
    round: u8,
    index: u64,
    stats: SearchStats,
    mode: Mode,
}

impl Selection {
    /// The winning plan.
    #[inline]
    #[must_use]
    pub const fn plan(&self) -> SchedulePlan {
        self.winner.plan
    }

    /// Its training `ΣT`, `ΣP`, `ΣZ`.
    #[inline]
    #[must_use]
    pub const fn train(&self) -> Totals {
        self.winner.totals
    }

    /// Who proposed it.
    #[inline]
    #[must_use]
    pub const fn proposer(&self) -> &ProposerId {
        &self.proposer
    }

    /// The round it was proposed in.
    #[inline]
    #[must_use]
    pub const fn round(&self) -> u8 {
        self.round
    }

    /// Its index among that round's proposals.
    #[inline]
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// What the search counted.
    #[inline]
    #[must_use]
    pub const fn stats(&self) -> SearchStats {
        self.stats
    }
}

/// The grid's own training selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GridSelection {
    /// The winning plan.
    pub(super) plan: SchedulePlan,
    /// The round it was proposed in.
    pub(super) round: u8,
    /// Its index among that round's proposals.
    pub(super) index: u64,
    /// What the search counted.
    pub(super) stats: SearchStats,
}

impl GridSelection {
    /// `s`'s plan, round, index and counts.
    #[inline]
    const fn of(s: &Selection) -> GridSelection {
        GridSelection {
            plan: s.winner.plan,
            round: s.round,
            index: s.index,
            stats: s.stats,
        }
    }
}

/// An admitted plan, with its bundle.
#[derive(Debug)]
#[cfg_attr(test, derive(Clone))]
pub struct Admitted {
    policy: ArtifactId,
    plan: SchedulePlan,
    bundle: CandidateBundle,
    useful: bool,
    gain: Gain,
}

impl Admitted {
    /// The policy it was admitted under.
    #[inline]
    #[must_use]
    pub const fn policy(&self) -> &ArtifactId {
        &self.policy
    }

    /// The plan.
    #[inline]
    #[must_use]
    pub const fn plan(&self) -> SchedulePlan {
        self.plan
    }

    /// Its provenance bundle.
    #[inline]
    #[must_use]
    pub const fn bundle(&self) -> &CandidateBundle {
        &self.bundle
    }

    /// Whether it is useful by the policy's rule.
    #[inline]
    #[must_use]
    pub const fn useful(&self) -> bool {
        self.useful
    }

    /// Its gain.
    #[inline]
    #[must_use]
    pub const fn gain(&self) -> Gain {
        self.gain
    }

    /// Its bundle, the result consumed.
    #[inline]
    #[must_use]
    pub fn into_bundle(self) -> CandidateBundle {
        self.bundle
    }

    /// Declare the caller's toolchain line (declared metadata, not checked or scored).
    pub fn declare_toolchain(&mut self, line: &str) {
        line.clone_into(&mut self.bundle.parts_mut().toolchain);
    }

    /// The result as if admitted under another `policy` (never built by the pinned policy).
    #[cfg(test)]
    pub(super) fn under_policy(mut self, policy: ArtifactId) -> Admitted {
        self.policy = policy;
        self
    }

    /// Records the lab's `measured` timing under `label`, never scored, or returns `false` and
    /// records nothing for an empty label or past [`MAX_TIMINGS`].
    #[must_use]
    pub fn record_timing(&mut self, label: &str, measured: Duration) -> bool {
        let timings = &mut self.bundle.parts_mut().timings;
        if label.is_empty() || wide(timings.len()) >= MAX_TIMINGS {
            return false;
        }
        timings.push(Timing {
            label: label.to_owned(),
            measured_ns: u64::try_from(measured.as_nanos()).unwrap_or(u64::MAX),
        });
        true
    }
}

/// How often the evaluator read each split (tests only).
#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct Reads {
    /// Training items judged.
    pub(super) train: AtomicU64,
    /// Held-out items judged.
    pub(super) held_out: AtomicU64,
    /// Held-out gates entered.
    pub(super) looks: AtomicU64,
}

/// A defect injected into one item's judgment, a run the evaluator must refuse by name.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// The golden trace's bytes replaced: a trace that does not decode, read or restore.
    GoldenBytes(Vec<u8>),
    /// The lane's calls compared with the host's log without their last one.
    Unlogged,
    /// The host's log replayed without its last input: a replay that does not reach the live head.
    ReplayShort,
}

/// The evaluator of the pinned policy v1.
#[derive(Debug)]
pub struct Evaluator {
    policy: AcceptancePolicy,
    corpus: Corpus,
    build: String,
    origin: Instant,
    /// The inputs stepped so far ([`Self::stepped`]).
    stepped: AtomicU64,
    /// The grid's own selection, once a search ran the grid ([`Self::grid_selection`]); `None`
    /// inside when the grid has no training winner.
    grid: OnceLock<Option<GridSelection>>,
    /// The M6 labels judged so far, by candidate id, for its life (design §13.3).
    #[cfg(any(test, debug_assertions, feature = "aos-learn"))]
    labels: Mutex<BTreeMap<Digest, TrainLabel>>,
    /// The most distinct plans it labels ([`MAX_JUDGED`]; lower in tests:
    /// `Evaluator::capping_labels`).
    #[cfg(any(test, debug_assertions, feature = "aos-learn"))]
    label_cap: usize,
    /// A held-out item's scenario replaced (tests only: `Evaluator::replacing`).
    #[cfg(test)]
    replaced: Option<(usize, Scenario)>,
    /// One item's judgment faulted (tests only: `Evaluator::faulting`).
    #[cfg(test)]
    fault: Option<(String, Fault)>,
    #[cfg(test)]
    reads: Reads,
}

impl Evaluator {
    /// Evaluator of [`AcceptancePolicy::pinned`] in `build` with runs offset from `origin`.
    /// # Errors
    /// [`PolicyRefusal::PolicyMutated`], [`PolicyRefusal::CorpusDrift`] or
    /// [`PolicyRefusal::BaselineDrift`], from the policy or from the corpus generated again here.
    pub fn v1(build: &str, origin: Instant) -> Result<Evaluator, PolicyRefusal> {
        let policy = AcceptancePolicy::pinned(origin)?;
        let corpus = Corpus::generate().map_err(|_| PolicyRefusal::CorpusDrift {
            item: CORPUS_FOLD.to_owned(),
        })?;
        let facts: Vec<ItemFacts<'_>> = corpus.items().iter().map(ItemFacts::of).collect();
        policy.check_corpus(&facts)?;
        Ok(Evaluator {
            policy,
            corpus,
            build: build.to_owned(),
            origin,
            stepped: AtomicU64::new(0),
            grid: OnceLock::new(),
            #[cfg(any(test, debug_assertions, feature = "aos-learn"))]
            labels: Mutex::new(BTreeMap::new()),
            #[cfg(any(test, debug_assertions, feature = "aos-learn"))]
            label_cap: MAX_JUDGED,
            #[cfg(test)]
            replaced: None,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            reads: Reads::default(),
        })
    }

    /// The policy it judges under.
    #[inline]
    #[must_use]
    pub const fn policy(&self) -> &AcceptancePolicy {
        &self.policy
    }

    /// The build it judges in.
    #[inline]
    #[must_use]
    pub fn build(&self) -> &str {
        &self.build
    }

    /// The digest of the corpus it judges on.
    #[inline]
    #[must_use]
    pub const fn corpus_digest(&self) -> Digest {
        self.corpus.digest()
    }

    /// How many inputs it has stepped since it was built, lane calls and replayed inputs both.
    #[inline]
    #[must_use]
    pub fn stepped(&self) -> u64 {
        self.stepped.load(Ordering::Relaxed)
    }

    /// Counts `n` inputs stepped.
    #[inline]
    fn count_stepped(&self, n: usize) {
        self.stepped.fetch_add(wide(n), Ordering::Relaxed);
    }

    /// A new evaluator of this one's build, policy and corpus, which [`Self::v1`] checked when it
    /// built this one, with nothing stepped, read, cached, replaced or faulted (tests only: a test
    /// counts its own reads without measuring the baseline again).
    #[cfg(test)]
    pub(super) fn anew(&self) -> Evaluator {
        Evaluator {
            policy: self.policy.clone(),
            corpus: self.corpus.clone(),
            build: self.build.clone(),
            origin: self.origin,
            stepped: AtomicU64::new(0),
            grid: OnceLock::new(),
            replaced: None,
            fault: None,
            reads: Reads::default(),
            labels: Mutex::new(BTreeMap::new()),
            label_cap: MAX_JUDGED,
        }
    }

    /// Whether a search has filled the grid's selection (tests only).
    #[cfg(test)]
    pub(super) fn grid_cached(&self) -> bool {
        self.grid.get().is_some()
    }

    /// The evaluator with the scenario of the held-out item `id` replaced by `s` (tests only).
    #[cfg(test)]
    pub(super) fn replacing(mut self, id: &str, s: Scenario) -> Evaluator {
        let at = self
            .corpus
            .items()
            .iter()
            .position(|i| i.id() == id && i.split() == CorpusSplit::HeldOut)
            .expect("fixture: a held-out item");
        self.replaced = Some((at, s));
        self
    }

    /// How often it read each split (tests only).
    #[cfg(test)]
    pub(super) const fn reads(&self) -> &Reads {
        &self.reads
    }

    /// The evaluator with at most `cap` distinct plans labelled (tests only).
    #[cfg(test)]
    pub(super) const fn capping_labels(mut self, cap: usize) -> Evaluator {
        self.label_cap = cap;
        self
    }

    /// The evaluator with the judgment of item `id` faulted by `fault` (tests only).
    #[cfg(test)]
    pub(super) fn faulting(mut self, id: &str, fault: Fault) -> Evaluator {
        assert!(
            self.corpus.items().iter().any(|i| i.id() == id),
            "fixture: a corpus item"
        );
        self.fault = Some((id.to_owned(), fault));
        self
    }

    /// The fault injected into item `id`'s judgment, if any (tests only).
    #[cfg(test)]
    fn fault_at(&self, id: &str) -> Option<&Fault> {
        self.fault
            .as_ref()
            .filter(|(at, _)| at == id)
            .map(|(_, fault)| fault)
    }

    fn facts(&self, k: usize) -> Option<ItemFacts<'_>> {
        let item = self.corpus.items().get(k)?;
        #[cfg(test)]
        if let Some((at, s)) = &self.replaced
            && *at == k
        {
            return Some(ItemFacts {
                body: Body::Lane(s),
                ..ItemFacts::of(item)
            });
        }
        #[cfg(test)]
        if let Some(Fault::GoldenBytes(bytes)) = self.fault_at(item.id()) {
            return Some(ItemFacts {
                body: Body::Trace(bytes),
                ..ItemFacts::of(item)
            });
        }
        Some(ItemFacts::of(item))
    }

    fn indices(&self, split: CorpusSplit) -> Vec<usize> {
        self.corpus
            .items()
            .iter()
            .enumerate()
            .filter(|(_, i)| i.split() == split)
            .map(|(k, _)| k)
            .collect()
    }

    /// Evaluate item `k`'s row under `sched` and return any refusal that stops a candidate there.
    pub(super) fn judge(&self, sched: Sched, k: usize) -> Option<(Row, Option<CandidateRefusal>)> {
        let f = self.facts(k)?;
        #[cfg(any(test, debug_assertions, feature = "aos-learn"))]
        if f.split == CorpusSplit::HeldOut {
            HELD_OUT_JUDGED.fetch_add(1, Ordering::SeqCst);
        }
        #[cfg(test)]
        match f.split {
            CorpusSplit::Train => self.reads.train.fetch_add(1, Ordering::Relaxed),
            CorpusSplit::HeldOut => self.reads.held_out.fetch_add(1, Ordering::Relaxed),
        };
        Some(match f.body {
            Body::Trace(bytes) => self.judge_trace(sched, &f, bytes),
            Body::Lane(s) => self.judge_lane(sched, &f, s),
        })
    }

    /// Item `k`'s row under the planned code of `plan`.
    pub(super) fn row_at(&self, plan: SchedulePlan, k: usize) -> Option<Row> {
        self.judge(Sched::Plan(plan), k).map(|(row, _)| row)
    }

    fn judge_trace(
        &self,
        sched: Sched,
        f: &ItemFacts<'_>,
        bytes: &[u8],
    ) -> (Row, Option<CandidateRefusal>) {
        let mut row = blank(f);
        row.run = RowVerdict::Pass;
        let unreadable = |mut row: Row, detail: String| {
            row.run = RowVerdict::fail(GOLDEN_UNREADABLE, 0);
            let refusal = CandidateRefusal::Entry {
                item: row.item.clone(),
                law: GOLDEN_UNREADABLE,
                detail,
            };
            (row, Some(refusal))
        };
        let trace = match MomentumTrace::decode(bytes) {
            Ok(t) => t,
            Err(e) => return unreadable(row, e.to_string()),
        };
        let read = match trace.read(self.origin) {
            Ok(r) => r,
            Err(e) => return unreadable(row, e.to_string()),
        };
        let start = match read.checkpoint.restore(&read.schema, &trace.identity) {
            Ok(s) => s,
            Err(e) => return unreadable(row, e.to_string()),
        };
        let code = PrebuiltMomentum::of(sched, &self.build);
        let first = read.log.header().first_cursor;
        if let Err(r) = code.entry(&read.schema, &start) {
            row.run = RowVerdict::fail(r.law, first);
            let refusal = CandidateRefusal::Entry {
                item: row.item.clone(),
                law: r.law,
                detail: r.detail,
            };
            return (row, Some(refusal));
        }
        let replayed = replay(
            &read.schema,
            &code,
            start,
            read.log.inputs(),
            first,
            &mut NoEffects,
        );
        self.count_stepped(read.log.inputs().len());
        let (mut ticks, mut presented) = (0u64, 0u64);
        let mut glass = Enc::new();
        encode_glass(&mut glass, &MomentumGlowFrame::default(), &[], &[]);
        let mut drawn = Enc::new();
        for (_, o) in &replayed.observations {
            if let MomentumObservation::Frame { .. } = o {
                ticks += 1;
                drawn.clear();
                read.schema.encode_observation(o, &mut drawn);
                if drawn.as_bytes() != glass.as_bytes() {
                    presented += 1;
                    core::mem::swap(&mut glass, &mut drawn);
                }
            }
        }
        row.chain = replayed.observation_chain;
        row.ticks = ticks;
        row.presented = presented;
        row.wasted = ticks.saturating_sub(presented);
        let verdict = NoStrandedLight::new().judge(&read.schema, read.log.inputs(), &replayed);
        row.stranded_light = row_verdict(&verdict);
        let refusal = failed(&verdict).map(|(law, cursor, detail)| CandidateRefusal::OpenLoop {
            item: row.item.clone(),
            law,
            cursor,
            detail,
        });
        (row, refusal)
    }

    fn judge_lane(
        &self,
        sched: Sched,
        f: &ItemFacts<'_>,
        s: &Scenario,
    ) -> (Row, Option<CandidateRefusal>) {
        let mut h = MomentumHost::default();
        match sched {
            Sched::Plan(plan) => {
                h.attach_planned(self.origin, plan, &self.build, 1, MOMENTUM_MAX_EVENTS);
            }
            Sched::Prebuilt(code) => h.attach_with(
                self.origin,
                code,
                &self.build,
                1,
                MOMENTUM_QUEUE_CAP,
                MOMENTUM_MAX_EVENTS,
            ),
        }
        let r = run(&mut h, s, self.origin);
        self.count_stepped(r.calls().len());
        let mut fidelity = LaneFidelity::new().judge(&r);
        if let (Verdict::Pass, Some(fault)) = (&fidelity, r.fault()) {
            fidelity = Verdict::Fail {
                cursor: wide(r.ticks().len()),
                law: LANE_SETTLES,
                detail: format!("the lane stopped before the scenario's end: {fault}"),
            };
        }
        let c = r.counts();
        let mut row = blank(f);
        row.lane_fidelity = row_verdict(&fidelity);
        row.lane = Some(r.fnv());
        row.ticks = c.ticks;
        row.presented = c.presented;
        row.wasted = c.wasted();
        let item = || f.id.to_owned();
        let not_logged = |mut row: Row| {
            row.run = RowVerdict::fail(LANE_NOT_LOGGED, 0);
            (row, Some(CandidateRefusal::LaneNotLogged { item: item() }))
        };
        let HostMode::Hosted(hd) = &h.mode else {
            return not_logged(row);
        };
        row.firings = Some(hd.guard.firings);
        let b = &hd.boundary;
        let (Some(checkpoint), Some(log)) = (b.checkpoint(), b.log()) else {
            return not_logged(row);
        };
        let calls = r.calls();
        #[cfg(test)]
        let calls = match self.fault_at(f.id) {
            Some(Fault::Unlogged) => calls.split_last().map_or(calls, |(_, rest)| rest),
            _ => calls,
        };
        if log.broken().is_some() || log.inputs() != calls {
            return not_logged(row);
        }
        let first = log.header().first_cursor;
        let Ok(start) = checkpoint.restore(b.schema(), b.identity()) else {
            row.run = RowVerdict::fail(LANE_NOT_REPRODUCED, first);
            return (
                row,
                Some(CandidateRefusal::LaneNotReproduced { item: item() }),
            );
        };
        let inputs = log.inputs();
        #[cfg(test)]
        let inputs = match self.fault_at(f.id) {
            Some(Fault::ReplayShort) => inputs.split_last().map_or(inputs, |(_, rest)| rest),
            _ => inputs,
        };
        let replayed = replay(
            b.schema(),
            &b.pair().code,
            start,
            inputs,
            first,
            &mut NoEffects,
        );
        self.count_stepped(inputs.len());
        row.chain = replayed.observation_chain;
        let verdict = NoStrandedLight::new().judge(b.schema(), inputs, &replayed);
        row.stranded_light = row_verdict(&verdict);
        let reproduced = replayed.refused.is_none()
            && replayed.observation_chain == hd.live_chain()
            && replayed.state_digest == state_digest(b.schema(), hd.state());
        if !reproduced {
            row.run = RowVerdict::fail(LANE_NOT_REPRODUCED, first);
            return (
                row,
                Some(CandidateRefusal::LaneNotReproduced { item: item() }),
            );
        }
        row.run = RowVerdict::Pass;
        if let Some((law, cursor, detail)) = failed(&verdict) {
            let refusal = CandidateRefusal::StrandedLight {
                item: item(),
                law,
                cursor,
                detail,
            };
            return (row, Some(refusal));
        }
        let refusal =
            failed(&fidelity).map(|(law, cursor, detail)| CandidateRefusal::LaneFidelity {
                item: item(),
                law,
                cursor,
                detail,
            });
        (row, refusal)
    }

    /// `plan`'s training rows, in corpus order up to its first refusal, whose row is the last;
    /// after each passing item, `keep` is given its training ordinal and may stop the run.
    pub(super) fn training_rows<B>(
        &self,
        plan: SchedulePlan,
        keep: &mut dyn FnMut(usize) -> ControlFlow<B>,
    ) -> TrainingRows<B> {
        let mut rows = Vec::new();
        for (ordinal, k) in self.indices(CorpusSplit::Train).into_iter().enumerate() {
            let Some((row, refusal)) = self.judge(Sched::Plan(plan), k) else {
                continue;
            };
            rows.push((k, row));
            if let Some(refusal) = refusal {
                return TrainingRows {
                    rows,
                    end: TrainingEnd::Refused(refusal),
                };
            }
            if let ControlFlow::Break(why) = keep(ordinal) {
                return TrainingRows {
                    rows,
                    end: TrainingEnd::Stopped(why),
                };
            }
        }
        TrainingRows {
            rows,
            end: TrainingEnd::Admitted,
        }
    }

    fn train(&self, bytes: &[u8]) -> Result<TrainAdmitted, CandidateRefusal> {
        let plan = self.decode(bytes)?;
        let TrainingRows { rows, end } =
            self.training_rows::<Infallible>(plan, &mut |_| ControlFlow::Continue(()));
        match end {
            TrainingEnd::Admitted => Ok(TrainAdmitted::of(plan, rows)),
            TrainingEnd::Refused(refusal) => Err(refusal),
            TrainingEnd::Stopped(never) => match never {},
        }
    }

    fn decode(&self, bytes: &[u8]) -> Result<SchedulePlan, CandidateRefusal> {
        let limit = self.policy.budgets().proposal_bytes;
        if bytes.len() > limit {
            return Err(CandidateRefusal::Budget {
                len: bytes.len(),
                limit,
            });
        }
        SchedulePlan::decode(bytes).map_err(CandidateRefusal::Decode)
    }

    /// Select the training winner among `proposer`'s proposals (lowest `ΣT`, `ΣZ`, then bytes).
    /// # Errors
    /// [`NoOutcome::NoneAdmitted`], or [`NoOutcome::NoGain`] when the winner does not beat the
    /// baseline on training.
    pub fn select(&self, proposer: &mut dyn Proposer) -> Result<Selection, NoOutcome> {
        self.select_spread(proposer, &Serial)
    }

    /// [`Self::select`], each round's distinct candidates trained through `spread` and taken in
    /// proposal order: the same selection, counts and feedback whatever the spread runs.
    /// # Errors
    /// [`Self::select`]'s.
    pub fn select_spread(
        &self,
        proposer: &mut dyn Proposer,
        spread: &dyn Spread,
    ) -> Result<Selection, NoOutcome> {
        let (selected, grid) = self.select_local(proposer, spread);
        if grid {
            let _ = self.grid.set(selected.as_ref().ok().map(GridSelection::of));
        }
        selected
    }

    /// Return the grid's selection, running the grid search once per evaluator if needed.
    pub(super) fn grid_selection(&self) -> Option<GridSelection> {
        *self.grid.get_or_init(|| {
            self.select_local(&mut GridProposer::new(), &Serial)
                .0
                .as_ref()
                .ok()
                .map(GridSelection::of)
        })
    }

    fn select_local(
        &self,
        proposer: &mut dyn Proposer,
        spread: &dyn Spread,
    ) -> (Result<Selection, NoOutcome>, bool) {
        let budgets = self.policy.budgets();
        let mut fold = Fold::new(proposer.id());
        for round in 0..budgets.rounds {
            let proposed = fold.propose(proposer, round);
            let distinct = fold.distinct(&budgets, &proposed);
            let trained = spread_map(spread, &distinct, &|d| self.train(d.bytes));
            for (d, judged) in distinct.iter().zip(trained) {
                fold.take(
                    round,
                    d.index,
                    d.cid,
                    judged.map_or_else(Judged::Refused, Judged::Admitted),
                );
            }
        }
        fold.end(self, Mode::Search)
    }

    /// A distributed search of `proposer`'s round-0 candidates: the jobs workers run, and the
    /// claims they return, folded into a selection only by [`Distributed::finish`].
    pub fn distributed<'a>(
        &'a self,
        proposer: &'a mut dyn Proposer,
        check: CheckLevel,
    ) -> Distributed<'a> {
        let budgets = self.policy.budgets();
        let mut fold = Fold::new(proposer.id());
        let proposed = if budgets.rounds > 0 {
            fold.propose(proposer, 0)
        } else {
            Vec::new()
        };
        let mut jobs = Vec::new();
        let distinct = fold
            .distinct(&budgets, &proposed)
            .into_iter()
            .map(|d| {
                let at = self.decode(d.bytes).map(|plan| {
                    let slot = u32::try_from(jobs.len()).unwrap_or(u32::MAX);
                    jobs.push(Job { slot, plan });
                    slot
                });
                (d.index, d.cid, at)
            })
            .collect();
        Distributed {
            claims: jobs.iter().map(|_| None).collect(),
            ev: self,
            proposer,
            check,
            fold,
            rounds: budgets.rounds,
            distinct,
            jobs,
        }
    }

    /// Training ordinal `t` of `plan` judged here and compared with `claim`'s row `t`.
    fn judge_claimed(
        &self,
        plan: SchedulePlan,
        claim: &Claim,
        train: &[usize],
        t: usize,
    ) -> Result<(usize, Row, Option<CandidateRefusal>), BundleField> {
        let k = *train.get(t).ok_or(BundleField::Row)?;
        let (row, refusal) = self.judge(Sched::Plan(plan), k).ok_or(BundleField::Row)?;
        claim.check(t, &row)?;
        let claimed_refusal = claim.kind() == ClaimKind::Refused && t + 1 == claim.len();
        if refusal.is_some() == claimed_refusal {
            Ok((k, row, refusal))
        } else {
            Err(BundleField::Run)
        }
    }

    /// Design §11.3's step 5 on `claims`, one per job: every refused claim judged on its
    /// prefix, the best admitted claim in full, then each other admitted claim until its judged
    /// rows' `ΣT` exceed the winner's, or in full under [`CheckLevel::All`]. Each slot's verdict,
    /// or the first contradiction by slot and item; and the (slot, ordinal) pairs judged.
    fn check_claims(
        &self,
        jobs: &[Job],
        claims: &[Claim],
        check: CheckLevel,
        spread: &dyn Spread,
    ) -> Checked {
        let train = self.indices(CorpusSplit::Train);
        let best = jobs
            .iter()
            .zip(claims)
            .filter(|(_, c)| c.kind() == ClaimKind::Admitted)
            .min_by_key(|(j, c)| {
                let t = c.totals();
                (t.ticks, t.wasted, j.plan.encode())
            })
            .map(|(j, _)| j.slot);
        let mut tasks = Vec::new();
        for (j, c) in jobs.iter().zip(claims) {
            match c.kind() {
                ClaimKind::Refused => tasks.push(Task::Prefix(j.slot)),
                ClaimKind::Admitted if Some(j.slot) == best || check == CheckLevel::All => {
                    tasks.extend((0..c.len()).map(|t| Task::Item(j.slot, t)));
                }
                ClaimKind::Admitted => {}
            }
        }
        let first = spread_map(spread, &tasks, &|task| {
            self.run_task(*task, jobs, claims, &train, 0)
        });
        let mut judged: Vec<(u32, usize)> = first.iter().flat_map(|o| o.judged.clone()).collect();
        let mut done: Vec<(Task, Result<Done, Contradiction>)> = tasks
            .into_iter()
            .zip(first.into_iter().map(|o| o.done))
            .collect();
        if let Some(c) = first_contradiction(&done) {
            return (Err(c), judged);
        }
        let mut rows: Vec<Vec<(usize, Row)>> = jobs.iter().map(|_| Vec::new()).collect();
        let mut refused: Vec<Option<CandidateRefusal>> = jobs.iter().map(|_| None).collect();
        for (task, out) in done.drain(..) {
            match (task, out) {
                (Task::Prefix(slot), Ok(Done::Refused(r))) => {
                    if let Some(at) = refused.get_mut(index(slot)) {
                        *at = Some(r);
                    }
                }
                (Task::Item(slot, _), Ok(Done::Row(k, row))) => {
                    if let Some(at) = rows.get_mut(index(slot)) {
                        at.push((k, row));
                    }
                }
                _ => {}
            }
        }
        let winner_ticks = best.and_then(|slot| rows.get(index(slot))).map(|r| {
            r.iter()
                .fold(0u64, |t, (_, row)| t.saturating_add(row.ticks))
        });
        let mut dominated: Vec<bool> = jobs.iter().map(|_| false).collect();
        if let (CheckLevel::Dominance, Some(winner)) = (check, winner_ticks) {
            let rest: Vec<Task> = jobs
                .iter()
                .zip(claims)
                .filter(|(j, c)| c.kind() == ClaimKind::Admitted && Some(j.slot) != best)
                .map(|(j, _)| Task::Dominate(j.slot))
                .collect();
            let second = spread_map(spread, &rest, &|task| {
                self.run_task(*task, jobs, claims, &train, winner)
            });
            judged.extend(second.iter().flat_map(|o| o.judged.clone()));
            let done: Vec<(Task, Result<Done, Contradiction>)> = rest
                .into_iter()
                .zip(second.into_iter().map(|o| o.done))
                .collect();
            if let Some(c) = first_contradiction(&done) {
                return (Err(c), judged);
            }
            for (task, out) in done {
                match (task, out) {
                    (Task::Dominate(slot), Ok(Done::Dominated)) => {
                        if let Some(at) = dominated.get_mut(index(slot)) {
                            *at = true;
                        }
                    }
                    (Task::Dominate(slot), Ok(Done::Rows(all))) => {
                        if let Some(at) = rows.get_mut(index(slot)) {
                            *at = all;
                        }
                    }
                    _ => {}
                }
            }
        }
        let verdicts = jobs
            .iter()
            .zip(claims)
            .map(|(j, c)| {
                let slot = index(j.slot);
                match (c.kind(), refused.get_mut(slot).and_then(Option::take)) {
                    (ClaimKind::Refused, Some(r)) => Ok(Judged::Refused(r)),
                    (ClaimKind::Admitted, None) if dominated.get(slot) == Some(&true) => {
                        Ok(Judged::Dominated {
                            ticks: c.totals().ticks,
                        })
                    }
                    (ClaimKind::Admitted, None) => {
                        let mut r = rows.get_mut(slot).map(core::mem::take).unwrap_or_default();
                        r.sort_by_key(|&(k, _)| k);
                        if r.len() == c.len() {
                            Ok(Judged::Admitted(TrainAdmitted::of(j.plan, r)))
                        } else {
                            Err(Contradiction::unjudged(j.slot))
                        }
                    }
                    (ClaimKind::Refused, None) | (ClaimKind::Admitted, Some(_)) => {
                        Err(Contradiction::unjudged(j.slot))
                    }
                }
            })
            .collect::<Result<Vec<Judged>, Contradiction>>();
        (verdicts, judged)
    }

    /// One task of step 5, `winner` being the winner's local `ΣT` for [`Task::Dominate`].
    fn run_task(
        &self,
        task: Task,
        jobs: &[Job],
        claims: &[Claim],
        train: &[usize],
        winner: u64,
    ) -> TaskOut {
        let slot = match task {
            Task::Prefix(s) | Task::Item(s, _) | Task::Dominate(s) => s,
        };
        let (Some(job), Some(claim)) = (jobs.get(index(slot)), claims.get(index(slot))) else {
            return TaskOut {
                done: Err(Contradiction::unjudged(slot)),
                judged: Vec::new(),
            };
        };
        let mut judged = Vec::new();
        let contradicted = |t: usize, field: BundleField, judged: Vec<(u32, usize)>| TaskOut {
            done: Err(Contradiction {
                slot,
                ordinal: t,
                item: claim_item(self, train, t),
                field,
            }),
            judged,
        };
        match task {
            Task::Prefix(_) => {
                for t in 0..claim.len() {
                    judged.push((slot, t));
                    match self.judge_claimed(job.plan, claim, train, t) {
                        Err(field) => return contradicted(t, field, judged),
                        Ok((_, _, Some(r))) => {
                            return TaskOut {
                                done: Ok(Done::Refused(r)),
                                judged,
                            };
                        }
                        Ok((_, _, None)) => {}
                    }
                }
                contradicted(claim.len(), BundleField::Run, judged)
            }
            Task::Item(_, t) => {
                judged.push((slot, t));
                match self.judge_claimed(job.plan, claim, train, t) {
                    Ok((k, row, None)) => TaskOut {
                        done: Ok(Done::Row(k, row)),
                        judged,
                    },
                    Ok((_, _, Some(_))) => contradicted(t, BundleField::Run, judged),
                    Err(field) => contradicted(t, field, judged),
                }
            }
            Task::Dominate(_) => {
                let mut order: Vec<usize> = (0..claim.len()).collect();
                order.sort_by_key(|&t| (core::cmp::Reverse(claim.ticks(t)), t));
                let mut sum = 0u64;
                let mut rows = Vec::new();
                for t in order {
                    judged.push((slot, t));
                    match self.judge_claimed(job.plan, claim, train, t) {
                        Ok((k, row, None)) => {
                            sum = sum.saturating_add(row.ticks);
                            rows.push((k, row));
                            if sum > winner {
                                return TaskOut {
                                    done: Ok(Done::Dominated),
                                    judged,
                                };
                            }
                        }
                        Ok((_, _, Some(_))) => return contradicted(t, BundleField::Run, judged),
                        Err(field) => return contradicted(t, field, judged),
                    }
                }
                TaskOut {
                    done: Ok(Done::Rows(rows)),
                    judged,
                }
            }
        }
    }

    /// The held-out gate: `selection`'s winner on every held-out item, once, then the result.
    /// # Errors
    /// [`NoOutcome::HeldOut`] at the first held-out refusal (no runner-up is tried), or
    /// [`NoOutcome::Foreign`] for a selection made in another build.
    pub fn held_out(&self, selection: Selection) -> Result<Admitted, NoOutcome> {
        if selection.build != self.build {
            return Err(NoOutcome::Foreign {
                build: selection.build,
            });
        }
        let plan = selection.winner.plan;
        self.gate(selection)
            .map_err(|refusal| NoOutcome::HeldOut { plan, refusal })
    }

    /// [`Self::select`], then [`Self::held_out`].
    /// # Errors
    /// Either one's.
    pub fn search(&self, proposer: &mut dyn Proposer) -> Result<Admitted, NoOutcome> {
        self.held_out(self.select(proposer)?)
    }

    /// The same pipeline for the one candidate `bytes`, training then held out.
    /// # Errors
    /// The first [`CandidateRefusal`], on either split.
    pub fn admit_one(&self, bytes: &[u8]) -> Result<Admitted, CandidateRefusal> {
        let winner = self.train(bytes)?;
        self.gate(Selection {
            build: self.build.clone(),
            winner,
            proposer: ProposerId::on_demand(),
            round: 0,
            index: 0,
            stats: SearchStats::ONE,
            mode: Mode::OnDemand,
        })
    }

    fn gate(&self, selection: Selection) -> Result<Admitted, CandidateRefusal> {
        #[cfg(test)]
        self.reads.looks.fetch_add(1, Ordering::Relaxed);
        let Selection {
            winner,
            proposer,
            round,
            index,
            stats,
            mode,
            ..
        } = selection;
        let plan = winner.plan;
        let mut rows = winner.rows;
        for k in self.indices(CorpusSplit::HeldOut) {
            let Some((row, refusal)) = self.judge(Sched::Plan(plan), k) else {
                continue;
            };
            if let Some(refusal) = refusal {
                return Err(refusal);
            }
            rows.push((k, row));
        }
        rows.sort_by_key(|&(k, _)| k);
        let rows: Vec<Row> = rows.into_iter().map(|(_, r)| r).collect();
        let s = sums(&self.policy, &rows);
        let useful = s.held_out.beats();
        let gain = Gain {
            held_out: s.held_out,
            s0: s.s0,
        };
        let parts = UncheckedBundle {
            policy: self.policy.id().clone(),
            base: MomentumCode::Shipped.artifact(&self.build),
            candidate: plan.artifact(&self.build),
            plan: plan.encode().to_vec(),
            proposer,
            round,
            index,
            stranded_light: NoStrandedLight::artifact(),
            lane_fidelity: LaneFidelity::artifact(),
            interpreter: PLAN_INTERPRETER.to_owned(),
            lane: LANE_MODEL.to_owned(),
            oracle: ORACLE.to_owned(),
            corpus: self.corpus.digest(),
            schema: schema_artifact(),
            profile: profile_artifact(),
            build: self.build.clone(),
            toolchain: UNDECLARED.to_owned(),
            rows,
            selection: SelectionPart {
                mode,
                stats,
                held_out_looks: 1,
                train: s.train,
                held_out: s.held_out,
                s0: s.s0,
                useful,
                gain_ppm: gain.ppm(),
                s0_gain_ppm: gain.s0_ppm(),
            },
            publication: None,
            timings: Vec::new(),
            grade: BUNDLE_GRADE.to_owned(),
            tested_assumptions: TESTED_ASSUMPTIONS.iter().map(|a| (*a).to_owned()).collect(),
            plan_assumptions: PLAN_ASSUMPTIONS.iter().map(|a| (*a).to_owned()).collect(),
        };
        Ok(Admitted {
            policy: self.policy.id().clone(),
            plan,
            bundle: CandidateBundle::computed(parts),
            useful,
            gain,
        })
    }
}

/// A plan's label on the training split (design §13.3): its id, the plan, and either its training
/// `ΣT`, `ΣP`, `ΣZ` or the first law it failed. It carries no held-out figure, and only
/// [`Evaluator::label_spread`] returns one (a copy of the evaluator's cached label).
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainLabel {
    id: Digest,
    plan: SchedulePlan,
    outcome: Result<Totals, &'static str>,
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl TrainLabel {
    /// `plan`'s label from its judgment on every training item, the one [`Evaluator::select`] makes.
    fn of(plan: SchedulePlan, judged: Result<TrainAdmitted, CandidateRefusal>) -> TrainLabel {
        TrainLabel {
            id: plan.digest(),
            plan,
            outcome: judged.map(|t| t.totals).map_err(|r| r.law()),
        }
    }

    /// The candidate's id ([`candidate_id`]).
    #[inline]
    #[must_use]
    pub const fn id(&self) -> Digest {
        self.id
    }

    /// The plan.
    #[inline]
    #[must_use]
    pub const fn plan(&self) -> SchedulePlan {
        self.plan
    }

    /// Its training `ΣT`, `ΣP`, `ΣZ`, if it was admitted on every training item.
    #[inline]
    #[must_use]
    pub const fn totals(&self) -> Option<Totals> {
        match self.outcome {
            Ok(totals) => Some(totals),
            Err(_) => None,
        }
    }

    /// The first law it failed on the training split, if it was refused.
    #[inline]
    #[must_use]
    pub const fn law(&self) -> Option<&'static str> {
        match self.outcome {
            Ok(_) => None,
            Err(law) => Some(law),
        }
    }
}

/// The capability to open a real pair's sets and mint their tokens (design §13.3). Only the lab's
/// session entry builds one, and a workspace source scan confines the name.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
pub struct RealSets(
    #[expect(
        dead_code,
        reason = "read by nothing: it makes the zero bit pattern invalid, so `mem::zeroed` panics"
    )]
    NonZeroU64,
);

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl RealSets {
    /// The capability, for the lab's session entry alone.
    #[must_use]
    #[allow(clippy::new_without_default)]
    pub const fn new() -> RealSets {
        RealSets(NonZeroU64::MIN)
    }
}

/// A rehearsal's or a test's set, which needs no capability: practice-held-out `j` or
/// practice-report `j`, and nothing else.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PracticeSet {
    /// Practice-held-out `j`, which look and replay tokens bind.
    HeldOut(u32),
    /// Practice-report `j`, which a read token binds.
    Report(u32),
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl PracticeSet {
    /// The set it names.
    #[inline]
    #[must_use]
    pub const fn set(self) -> SetId {
        match self {
            PracticeSet::HeldOut(index) => SetId {
                tag: SetTag::PracticeHeldOut,
                index,
            },
            PracticeSet::Report(index) => SetId {
                tag: SetTag::PracticeReport,
                index,
            },
        }
    }
}

/// The domain of a token's list digest, which the ledger line naming the list records.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
pub const LIST_DOMAIN: &[u8] = b"aterm-aos-m6-list-v1\0";

/// The digest of `list`, sorted and deduplicated, under [`LIST_DOMAIN`]: what a token over it
/// holds, so the ledger can name a list in its line before any token over it exists.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[must_use]
pub fn list_digest(list: &[[u8; PLAN_LEN]]) -> Digest {
    let mut list = list.to_vec();
    list.sort_unstable();
    list.dedup();
    let parts: Vec<&[u8]> = list.iter().map(<[u8; PLAN_LEN]>::as_slice).collect();
    Digest::of(LIST_DOMAIN, &parts)
}

/// Why a token was not minted.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintRefusal {
    /// The list is longer than the token's bound.
    TooLong {
        /// Its length.
        len: usize,
        /// The bound.
        cap: usize,
    },
    /// A plan of the list does not decode, or is not of the set's side.
    Side {
        /// The plan.
        plan: [u8; PLAN_LEN],
    },
    /// The token does not bind that kind of set.
    WrongSet {
        /// The set.
        set: SetId,
    },
    /// No v1 session opens the set.
    NoSuchSet {
        /// The set.
        set: SetId,
    },
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl fmt::Display for MintRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MintRefusal::TooLong { len, cap } => {
                write!(
                    f,
                    "token refused: a list of {len} plans, past its bound of {cap}"
                )
            }
            MintRefusal::Side { plan } => write!(
                f,
                "token refused: plan {} does not decode or is not of its set's side",
                Digest::of(super::plan::PLAN_DOMAIN, &[plan]).short()
            ),
            MintRefusal::WrongSet { set } => {
                write!(f, "token refused: it binds no {:?} set", set.tag)
            }
            MintRefusal::NoSuchSet { set } => {
                write!(
                    f,
                    "token refused: no v1 session opens {:?} {}",
                    set.tag, set.index
                )
            }
        }
    }
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl std::error::Error for MintRefusal {}

/// A token's binding: its set, and its plan list, sorted and deduplicated.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Debug, PartialEq, Eq)]
struct Bound {
    set: SetId,
    list: Vec<[u8; PLAN_LEN]>,
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl Bound {
    /// `list` bound to `set`, refused past `cap` plans or for a plan not of `set`'s side.
    fn mint(set: SetId, cap: usize, list: &[[u8; PLAN_LEN]]) -> Result<Bound, MintRefusal> {
        if list.len() > cap {
            return Err(MintRefusal::TooLong {
                len: list.len(),
                cap,
            });
        }
        if let Some(plan) = list.iter().find(|b| {
            SchedulePlan::decode(b.as_slice()).map_or(true, |p| side(&p) != set.tag.side())
        }) {
            return Err(MintRefusal::Side { plan: *plan });
        }
        let mut list = list.to_vec();
        list.sort_unstable();
        list.dedup();
        Ok(Bound { set, list })
    }

    /// `H_i` or `E_i` of v1, by `tag`.
    fn real(tag: SetTag, i: u32) -> Result<SetId, MintRefusal> {
        let set = SetId { tag, index: i };
        if sets_v1().contains(&set) {
            Ok(set)
        } else {
            Err(MintRefusal::NoSuchSet { set })
        }
    }

    /// The digest of its list ([`list_digest`]).
    fn digest(&self) -> Digest {
        list_digest(&self.list)
    }

    /// Whether `plans` is its list exactly (sorted and deduplicated, as the ledger line names
    /// it), each plan of its set's side and in one of its set's pools.
    fn admits(&self, plans: &[SchedulePlan]) -> Result<(), LabelRefusal> {
        let given: Vec<[u8; PLAN_LEN]> = plans.iter().map(SchedulePlan::encode).collect();
        if given != self.list {
            let stray = given.iter().find(|b| self.list.binary_search(b).is_err());
            return Err(LabelRefusal::ScopeMismatch {
                plan: stray.copied(),
            });
        }
        let pools = set_pools(self.set).map_err(LabelRefusal::Pool)?;
        let drawn: BTreeSet<[u8; PLAN_LEN]> =
            pools.iter().flatten().map(SchedulePlan::encode).collect();
        match plans
            .iter()
            .find(|p| side(p) != self.set.tag.side() || !drawn.contains(&p.encode()))
        {
            Some(p) => Err(LabelRefusal::ScopeMismatch {
                plan: Some(p.encode()),
            }),
            None => Ok(()),
        }
    }
}

/// The token a model-admission look labels its plan list under, once (design §13.3).
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Debug)]
pub struct LookToken {
    bound: Bound,
}

/// The token the report labels its set's plans under, once.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Debug)]
pub struct ReadToken {
    bound: Bound,
}

/// The token a replayed look labels its recomputed plan list under, once.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Debug)]
pub struct ReplayToken {
    bound: Bound,
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl LookToken {
    /// `H_i`'s look token over `list`, for a ledger holding the real sets' capability, minted after
    /// the `look-begin` line naming the list is durable.
    /// # Errors
    /// [`MintRefusal::NoSuchSet`], then [`MintRefusal::TooLong`] past [`LOOK_LIST_CAP`] plans or
    /// [`MintRefusal::Side`].
    pub(super) fn mint_real(
        _sets: &RealSets,
        i: u32,
        list: &[[u8; PLAN_LEN]],
    ) -> Result<LookToken, MintRefusal> {
        let set = Bound::real(SetTag::HeldOut, i)?;
        Bound::mint(set, LOOK_LIST_CAP, list).map(|bound| LookToken { bound })
    }

    /// A practice-held-out set's look token over `list`.
    /// # Errors
    /// [`MintRefusal::WrongSet`] for a practice-report set, then the bound's and the side's.
    pub(super) fn mint_practice(
        set: PracticeSet,
        list: &[[u8; PLAN_LEN]],
    ) -> Result<LookToken, MintRefusal> {
        match set {
            PracticeSet::HeldOut(_) => {
                Bound::mint(set.set(), LOOK_LIST_CAP, list).map(|bound| LookToken { bound })
            }
            PracticeSet::Report(_) => Err(MintRefusal::WrongSet { set: set.set() }),
        }
    }

    /// The set it binds.
    #[inline]
    #[must_use]
    pub const fn set(&self) -> SetId {
        self.bound.set
    }

    /// Its plan list, sorted and deduplicated.
    #[inline]
    #[must_use]
    pub fn list(&self) -> &[[u8; PLAN_LEN]] {
        &self.bound.list
    }

    /// Its list's digest, under [`LIST_DOMAIN`].
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.bound.digest()
    }
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl ReadToken {
    /// `E_i`'s read token over `list`, for a ledger holding the real sets' capability, minted after
    /// the `report-begin` line naming the list is durable.
    /// # Errors
    /// [`MintRefusal::NoSuchSet`], then [`MintRefusal::TooLong`] past [`READ_LIST_CAP`] plans or
    /// [`MintRefusal::Side`].
    pub(super) fn mint_real(
        _sets: &RealSets,
        i: u32,
        list: &[[u8; PLAN_LEN]],
    ) -> Result<ReadToken, MintRefusal> {
        let set = Bound::real(SetTag::Report, i)?;
        Bound::mint(set, READ_LIST_CAP, list).map(|bound| ReadToken { bound })
    }

    /// A practice-report set's read token over `list`.
    /// # Errors
    /// [`MintRefusal::WrongSet`] for a practice-held-out set, then the bound's and the side's.
    pub(super) fn mint_practice(
        set: PracticeSet,
        list: &[[u8; PLAN_LEN]],
    ) -> Result<ReadToken, MintRefusal> {
        match set {
            PracticeSet::Report(_) => {
                Bound::mint(set.set(), READ_LIST_CAP, list).map(|bound| ReadToken { bound })
            }
            PracticeSet::HeldOut(_) => Err(MintRefusal::WrongSet { set: set.set() }),
        }
    }

    /// The set it binds.
    #[inline]
    #[must_use]
    pub const fn set(&self) -> SetId {
        self.bound.set
    }

    /// Its plan list, sorted and deduplicated.
    #[inline]
    #[must_use]
    pub fn list(&self) -> &[[u8; PLAN_LEN]] {
        &self.bound.list
    }

    /// Its list's digest, under [`LIST_DOMAIN`].
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.bound.digest()
    }
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl ReplayToken {
    /// `H_i`'s replay token over a recorded look's recomputed `list`, for a ledger holding the real
    /// sets' capability.
    /// # Errors
    /// [`MintRefusal::NoSuchSet`], then [`MintRefusal::TooLong`] past [`LOOK_LIST_CAP`] plans or
    /// [`MintRefusal::Side`].
    pub(super) fn mint_real(
        _sets: &RealSets,
        i: u32,
        list: &[[u8; PLAN_LEN]],
    ) -> Result<ReplayToken, MintRefusal> {
        let set = Bound::real(SetTag::HeldOut, i)?;
        Bound::mint(set, LOOK_LIST_CAP, list).map(|bound| ReplayToken { bound })
    }

    /// A practice-held-out set's replay token over `list`.
    /// # Errors
    /// [`MintRefusal::WrongSet`] for a practice-report set, then the bound's and the side's.
    pub(super) fn mint_practice(
        set: PracticeSet,
        list: &[[u8; PLAN_LEN]],
    ) -> Result<ReplayToken, MintRefusal> {
        match set {
            PracticeSet::HeldOut(_) => {
                Bound::mint(set.set(), LOOK_LIST_CAP, list).map(|bound| ReplayToken { bound })
            }
            PracticeSet::Report(_) => Err(MintRefusal::WrongSet { set: set.set() }),
        }
    }

    /// The set it binds.
    #[inline]
    #[must_use]
    pub const fn set(&self) -> SetId {
        self.bound.set
    }

    /// Its plan list, sorted and deduplicated.
    #[inline]
    #[must_use]
    pub fn list(&self) -> &[[u8; PLAN_LEN]] {
        &self.bound.list
    }

    /// Its list's digest, under [`LIST_DOMAIN`].
    #[must_use]
    pub fn digest(&self) -> Digest {
        self.bound.digest()
    }
}

/// What one [`Evaluator::label_spread`] call may label (design §13.3).
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Debug)]
pub enum LabelScope {
    /// Side-0 plans and [`GRID_V1`]'s.
    Train,
    /// Practice set `j`'s side-3 plans, with no token (tests).
    Practice(u32),
    /// A look's plan list, exactly.
    HeldOut(LookToken),
    /// The report's plan list, exactly.
    Report(ReadToken),
    /// A replayed look's plan list, exactly.
    Replay(ReplayToken),
}

/// Why plans were not labelled.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelRefusal {
    /// A plan its scope does not take (`None`: the list is not its token's, though each plan of it
    /// is), before anything was judged.
    ScopeMismatch {
        /// The plan.
        plan: Option<[u8; PLAN_LEN]>,
    },
    /// A token's set did not draw its pools.
    Pool(PoolRefusal),
    /// Labelling them would take the evaluator past its cap of distinct plans labelled
    /// ([`MAX_JUDGED`]), before anything was judged.
    Budget {
        /// The distinct plans labelled already.
        labelled: usize,
        /// The new ones asked for.
        asked: usize,
        /// The cap.
        cap: usize,
    },
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl fmt::Display for LabelRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LabelRefusal::ScopeMismatch { plan: Some(plan) } => write!(
                f,
                "labelling refused: plan {} is outside its scope",
                Digest::of(super::plan::PLAN_DOMAIN, &[plan]).short()
            ),
            LabelRefusal::ScopeMismatch { plan: None } => {
                write!(f, "labelling refused: the list is not its token's")
            }
            LabelRefusal::Pool(e) => write!(f, "labelling refused: {e}"),
            LabelRefusal::Budget {
                labelled,
                asked,
                cap,
            } => write!(
                f,
                "labelling refused: {asked} new plans past the {labelled} labelled would pass the cap of {cap}"
            ),
        }
    }
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl std::error::Error for LabelRefusal {}

/// One pool's result under the selection rule (design §13.3): class 0 a selection that beats the
/// training baseline, 1 `NoGain` and 2 `NoneAdmitted`, with the winner and its training sums when
/// there is one. It is no [`Selection`], so [`Evaluator::held_out`] does not take one.
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolOutcome {
    class: u8,
    plan: Option<SchedulePlan>,
    totals: Totals,
    gain: (u64, u64),
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl PoolOutcome {
    /// 0 for a selection, 1 for `NoGain`, 2 for `NoneAdmitted`.
    #[inline]
    #[must_use]
    pub const fn class(&self) -> u8 {
        self.class
    }

    /// The training winner, unless none was admitted.
    #[inline]
    #[must_use]
    pub const fn plan(&self) -> Option<SchedulePlan> {
        self.plan
    }

    /// The winner's training `ΣT`, `ΣP`, `ΣZ` (zero when none was admitted).
    #[inline]
    #[must_use]
    pub const fn totals(&self) -> Totals {
        self.totals
    }

    /// `(class, ΣT, ΣZ)`, which admission compares, lower being better.
    #[inline]
    #[must_use]
    pub const fn key(&self) -> (u8, u64, u64) {
        (self.class, self.totals.ticks, self.totals.wasted)
    }

    /// Its `ΣT` and `ΣZ` gains: the training baseline's totals less the winner's for a
    /// selection, 0 otherwise (design §13.4).
    #[inline]
    #[must_use]
    pub const fn gain(&self) -> (u64, u64) {
        self.gain
    }
}

#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
impl Evaluator {
    /// Labels `plans` in their order, after every plan is checked against `scope`: the train scope
    /// takes side-0 and grid plans, a practice scope side-3 plans, and a token scope exactly its
    /// token's list, each plan of its set's side and pools. Only then is the cache read: each
    /// plan not labelled before is judged on every training item through `spread`, as
    /// [`Self::select`] judges it, and kept for the evaluator's life.
    /// # Errors
    /// [`LabelRefusal::ScopeMismatch`], a token set's [`LabelRefusal::Pool`] or
    /// [`LabelRefusal::Budget`], before anything is judged.
    pub fn label_spread(
        &self,
        scope: LabelScope,
        plans: &[[u8; PLAN_LEN]],
        spread: &dyn Spread,
    ) -> Result<Vec<TrainLabel>, LabelRefusal> {
        let decoded = plans
            .iter()
            .map(|b| {
                SchedulePlan::decode(b.as_slice())
                    .map_err(|_| LabelRefusal::ScopeMismatch { plan: Some(*b) })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let only = |takes: &dyn Fn(&SchedulePlan) -> bool| match decoded.iter().find(|p| !takes(p))
        {
            Some(p) => Err(LabelRefusal::ScopeMismatch {
                plan: Some(p.encode()),
            }),
            None => Ok(()),
        };
        match &scope {
            LabelScope::Train => only(&|p| {
                side(p) == Side::Train
                    || GRID_V1
                        .iter()
                        .any(|&(split, wake)| p.split() == split && p.wake() == wake)
            }),
            LabelScope::Practice(_) => only(&|p| side(p) == Side::Practice),
            LabelScope::HeldOut(t) => t.bound.admits(&decoded),
            LabelScope::Report(t) => t.bound.admits(&decoded),
            LabelScope::Replay(t) => t.bound.admits(&decoded),
        }?;
        let cache = || self.labels.lock().unwrap_or_else(PoisonError::into_inner);
        let new: Vec<SchedulePlan> = {
            let cached = cache();
            let mut seen = BTreeSet::new();
            decoded
                .iter()
                .filter(|p| !cached.contains_key(&p.digest()) && seen.insert(p.encode()))
                .copied()
                .collect()
        };
        let labelled = cache().len();
        if labelled.saturating_add(new.len()) > self.label_cap {
            return Err(LabelRefusal::Budget {
                labelled,
                asked: new.len(),
                cap: self.label_cap,
            });
        }
        let judged = spread_map(spread, &new, &|p| {
            TrainLabel::of(*p, self.train(&p.encode()))
        });
        let mut cached = cache();
        for label in judged {
            cached.entry(label.id).or_insert(label);
        }
        Ok(decoded
            .iter()
            .filter_map(|p| cached.get(&p.digest()).cloned())
            .collect())
    }

    /// The outcome of one pool's `K` ranked plans, folded by the selection rule.
    #[must_use]
    pub(super) fn pool_outcome(&self, labels: [&TrainLabel; JUDGED]) -> PoolOutcome {
        self.outcome(&labels)
    }

    /// The outcome of a whole pool, folded by the selection rule (the report's arm O alone).
    #[must_use]
    pub(super) fn oracle_outcome(&self, labels: &[&TrainLabel]) -> PoolOutcome {
        self.outcome(labels)
    }

    fn outcome(&self, labels: &[&TrainLabel]) -> PoolOutcome {
        let mut winner = Winner::new();
        for label in labels {
            if let Ok(totals) = label.outcome {
                winner.offer(totals, label.plan, label.plan);
            }
        }
        match winner.end(&self.policy) {
            None => PoolOutcome {
                class: 2,
                plan: None,
                totals: Totals::default(),
                gain: (0, 0),
            },
            Some((plan, train)) if train.beats() => PoolOutcome {
                class: 0,
                plan: Some(plan),
                totals: train.candidate,
                gain: (
                    train.baseline.ticks - train.candidate.ticks,
                    train.baseline.wasted - train.candidate.wasted,
                ),
            },
            Some((plan, train)) => PoolOutcome {
                class: 1,
                plan: Some(plan),
                totals: train.candidate,
                gain: (0, 0),
            },
        }
    }
}

fn blank(f: &ItemFacts<'_>) -> Row {
    Row {
        item: f.id.to_owned(),
        split: f.split,
        kind: f.kind,
        run: RowVerdict::NotRun,
        stranded_light: RowVerdict::NotRun,
        lane_fidelity: RowVerdict::NotRun,
        lane: None,
        chain: UNREPLAYED,
        ticks: 0,
        presented: 0,
        wasted: 0,
        firings: None,
    }
}

/// A verdict as a row records it.
pub(super) fn row_verdict(v: &Verdict) -> RowVerdict {
    match v {
        Verdict::Pass => RowVerdict::Pass,
        Verdict::Fail { cursor, law, .. } => RowVerdict::fail(law, *cursor),
    }
}

fn failed(v: &Verdict) -> Option<(&'static str, u64, String)> {
    match v {
        Verdict::Pass => None,
        Verdict::Fail {
            cursor,
            law,
            detail,
        } => Some((law, *cursor, detail.clone())),
    }
}
