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

use core::fmt;
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
    BUNDLE_GRADE, CandidateBundle, MAX_TIMINGS, PLAN_ASSUMPTIONS, Row, RowVerdict, SelectionPart,
    Timing, UNDECLARED, UncheckedBundle, add,
};
use super::corpus::{Corpus, CorpusSplit, ItemKind};
use super::fidelity::{LANE_SETTLES, LaneFidelity, ORACLE};
use super::lane::{LANE_MODEL, Scenario, encode_glass, run};
use super::plan::{PLAN_INTERPRETER, PLAN_LEN, PlanRefusal, SchedulePlan};
use super::policy::{AcceptancePolicy, Body, CORPUS_FOLD, ItemFacts, PolicyRefusal, Totals};
use super::search::{
    GridProposer, Mode, Proposer, ProposerId, SearchStats, TrainFeedback, candidate_id,
};
use super::{
    HostMode, MOMENTUM_MAX_EVENTS, MOMENTUM_QUEUE_CAP, MomentumCode, MomentumHost,
    MomentumObservation, MomentumTrace, NoStrandedLight, PrebuiltMomentum, Sched, profile_artifact,
    schema_artifact,
};
use crate::cursor_momentum::MomentumGlowFrame;

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
struct TrainAdmitted {
    plan: SchedulePlan,
    /// Its training rows, with their corpus indices.
    rows: Vec<(usize, Row)>,
    /// Their `ΣT`, `ΣP`, `ΣZ`.
    totals: Totals,
}

impl TrainAdmitted {
    /// The selection rule's key: `ΣT`, then `ΣZ`, then the plan's bytes.
    #[inline]
    fn key(&self) -> (u64, u64, [u8; PLAN_LEN]) {
        (self.totals.ticks, self.totals.wasted, self.plan.encode())
    }
}

/// The training winner, before the held-out split is read.
#[derive(Debug)]
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
    /// The grid's own selection, once a search ran the grid ([`Self::grid_selection`]): `None` inside when the grid has no training winner.
    grid: OnceLock<Option<GridSelection>>,
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

    fn train(&self, bytes: &[u8]) -> Result<TrainAdmitted, CandidateRefusal> {
        let plan = self.decode(bytes)?;
        let mut rows = Vec::new();
        for k in self.indices(CorpusSplit::Train) {
            let Some((row, refusal)) = self.judge(Sched::Plan(plan), k) else {
                continue;
            };
            if let Some(refusal) = refusal {
                return Err(refusal);
            }
            rows.push((k, row));
        }
        let totals = rows
            .iter()
            .fold(Totals::default(), |t, (_, r)| add(t, r.totals()));
        Ok(TrainAdmitted { plan, rows, totals })
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
        let (selected, grid) = self.select_noting_grid(proposer);
        if grid {
            let _ = self.grid.set(selected.as_ref().ok().map(GridSelection::of));
        }
        selected
    }

    /// Return the grid's selection, running the grid search once per evaluator if needed.
    pub(super) fn grid_selection(&self) -> Option<GridSelection> {
        *self.grid.get_or_init(|| {
            self.select_noting_grid(&mut GridProposer::new())
                .0
                .as_ref()
                .ok()
                .map(GridSelection::of)
        })
    }

    fn select_noting_grid(
        &self,
        proposer: &mut dyn Proposer,
    ) -> (Result<Selection, NoOutcome>, bool) {
        let budgets = self.policy.budgets();
        let id = proposer.id();
        let mut grid = GridProposer::new();
        let mut is_grid = id == grid.id();
        let mut stats = SearchStats::default();
        let mut seen: BTreeSet<Digest> = BTreeSet::new();
        let mut feedback: Vec<TrainFeedback> = Vec::new();
        let mut best: Option<(TrainAdmitted, u8, u64)> = None;
        for round in 0..budgets.rounds {
            stats.rounds = round + 1;
            let proposed = proposer.propose(round, &feedback);
            is_grid = is_grid && proposed == grid.propose(round, &feedback);
            for (index, bytes) in proposed.iter().enumerate() {
                stats.proposed += 1;
                if bytes.len() > budgets.proposal_bytes {
                    stats.oversized += 1;
                    continue;
                }
                let cid = candidate_id(bytes);
                if seen.contains(&cid) {
                    stats.duplicates += 1;
                    continue;
                }
                if seen.len() >= budgets.distinct {
                    stats.over_budget += 1;
                    continue;
                }
                seen.insert(cid);
                stats.distinct += 1;
                match self.train(bytes) {
                    Ok(t) => {
                        stats.admitted += 1;
                        feedback.push(TrainFeedback::admitted(cid, t.totals.ticks));
                        if best.as_ref().is_none_or(|(b, _, _)| t.key() < b.key()) {
                            best = Some((t, round, wide(index)));
                        }
                    }
                    Err(r) => feedback.push(TrainFeedback::refused(cid, r.law())),
                }
            }
        }
        let Some((winner, round, index)) = best else {
            return (Err(NoOutcome::NoneAdmitted { stats }), is_grid);
        };
        let train = Compared {
            candidate: winner.totals,
            baseline: self.policy.totals(CorpusSplit::Train),
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
            build: self.build.clone(),
            winner,
            proposer: id,
            round,
            index,
            stats,
            mode: Mode::Search,
        };
        (Ok(selection), is_grid)
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
