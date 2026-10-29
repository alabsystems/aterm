// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Aos M6 learner (design §13): a budgeted learned ranker under
//! `momentum-glow.m6-ranker-admission` v1. Its state and trainer, its data selection, the ledger
//! that orders every held-out and report read, the store that publishes only an admitted model,
//! and the session that runs them. It labels plans only through the evaluator's scoped
//! `label_spread`, so it never judges a held-out item.
//!
//! An admission cannot be written by hand:
//!
//! ```compile_fail,E0451
//! use aterm_effects::aos_momentum::m6::ModelAdmitted;
//! let _ = ModelAdmitted {
//!     pair: todo!(),
//!     look: 1,
//!     candidate: todo!(),
//!     comparator: todo!(),
//! };
//! ```
//!
//! nor copied:
//!
//! ```compile_fail,E0277
//! use aterm_effects::aos_momentum::m6::ModelAdmitted;
//! fn copy(a: &ModelAdmitted) -> ModelAdmitted { Clone::clone(a) }
//! ```
//!
//! and publishing one consumes it:
//!
//! ```compile_fail,E0382
//! use aterm_aos::Digest;
//! use aterm_effects::aos_momentum::m6::{DurableStore, ModelAdmitted, Store};
//! fn twice<S: DurableStore>(store: &mut Store<S>, a: ModelAdmitted, ledger: Digest) {
//!     let _ = store.activate(a, ledger);
//!     let _ = store.activate(a, ledger);
//! }
//! ```

use core::cmp::Ordering;
use core::fmt;
use core::fmt::Write;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use aterm_aos::Digest;
use aterm_aos::wire::{Dec, Enc, WireFault};

use super::corpus::SplitMix;
use super::m4::{
    Evaluator, LabelRefusal, LabelScope, LookToken, MintRefusal, PoolOutcome, PracticeSet,
    ReadToken, RealSets, ReplayToken, Spread, TrainLabel, list_digest,
};
use super::m6_policy::{
    EPOCHS, EXPLORE, FEATURES, JUDGED, LOOK_LEVEL, LOOK_LIST_CAP, LOOKS, M6_POLICY_V1_DIGEST,
    MAX_FIGURES_BYTES, MAX_LABELS, MAX_LINE_BYTES, MAX_REPORT_BYTES, MAX_REVISIONS, MAX_STEPS,
    MAX_VERSION_BYTES, MAX_VERSIONS, POOL_PLANS, POOLS, PoolId, PoolRefusal, READ_LIST_CAP,
    REPORT_LEVEL, ROUNDS, RankerPolicy, SetId, SetTag, VERSION_DOMAIN, base_digest, control_id,
    control_key, features, set_pools, tie_key, trainer_seed, w1_class, w2_class,
};
use super::plan::SchedulePlan;
use super::policy::GRID_V1;

/// The state encoding's magic.
pub const STATE_MAGIC: [u8; 4] = *b"AM6S";

/// The state encoding's format.
pub const STATE_FORMAT: u32 = 1;

/// The adapter's encoded length: `w`, [`FEATURES`] `i32`s.
pub const ADAPTER_LEN: usize = FEATURES * 4;

/// The optimizer state's encoded length: `acc` ([`FEATURES`] `i64`s), `t` and `epochs`.
pub const OPTIMIZER_LEN: usize = FEATURES * 8 + 8 + 4;

/// The random state's encoded length.
pub const RNG_LEN: usize = 8;

/// A state's encoded length: magic, format, `F`, adapter, optimizer, random state.
pub const STATE_LEN: usize = 4 + 4 + 4 + ADAPTER_LEN + OPTIMIZER_LEN + RNG_LEN;

const _: () = assert!(ADAPTER_LEN == 1_468 && OPTIMIZER_LEN == 2_948 && STATE_LEN == 4_436);

// The bounds (design §13.2): a step moves a weight by at most 1 and `t` counts the steps, so
// |w| <= t < 2^24, |acc| <= t(t+1)/2 < 2^48 and |score| < 3 * 2^48 < 2^50 (a plan has three
// active features).
const _: () = assert!(MAX_STEPS < 1 << 24);
const _: () = assert!(MAX_STEPS * (MAX_STEPS + 1) / 2 < 1 << 48);

/// The frozen base `B`, all zero, whose digest is [`base_digest`].
const BASE: [i32; FEATURES] = [0; FEATURES];

/// Why state bytes are not a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateRefusal {
    /// Not [`STATE_LEN`] bytes.
    Length {
        /// The length found.
        len: usize,
    },
    /// Not [`STATE_MAGIC`].
    Magic,
    /// Another format.
    Format {
        /// The format found.
        found: u32,
    },
    /// Another feature count.
    Features {
        /// The count found.
        found: u32,
    },
    /// `t` past [`MAX_STEPS`], or a weight or average past what `t` steps reach.
    Bounds,
}

impl fmt::Display for StateRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateRefusal::Length { len } => {
                write!(f, "state refused: {len} bytes, not {STATE_LEN}")
            }
            StateRefusal::Magic => write!(f, "state refused: not an M6 state"),
            StateRefusal::Format { found } => {
                write!(f, "state refused: format {found}, not {STATE_FORMAT}")
            }
            StateRefusal::Features { found } => {
                write!(f, "state refused: {found} features, not {FEATURES}")
            }
            StateRefusal::Bounds => write!(
                f,
                "state refused: its step count or a weight is past what its steps can reach"
            ),
        }
    }
}

impl std::error::Error for StateRefusal {}

/// A version's learned state (design §13.2): the adapter `w`, the optimizer's averaging buffer
/// `acc`, its step and epoch counts, and the trainer's random state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    w: [i32; FEATURES],
    acc: [i64; FEATURES],
    t: u64,
    epochs: u32,
    rng: u64,
}

impl State {
    /// v0's state: every weight and average 0, no step taken, the random state at
    /// [`trainer_seed`].
    #[inline]
    #[must_use]
    pub fn base() -> State {
        State {
            w: [0; FEATURES],
            acc: [0; FEATURES],
            t: 0,
            epochs: 0,
            rng: trainer_seed(),
        }
    }

    /// The pair steps taken across its chain.
    #[inline]
    #[must_use]
    pub const fn steps(&self) -> u64 {
        self.t
    }

    /// The epochs run across its chain.
    #[inline]
    #[must_use]
    pub const fn epochs(&self) -> u32 {
        self.epochs
    }

    /// The trainer's random state.
    #[inline]
    #[must_use]
    pub const fn random(&self) -> u64 {
        self.rng
    }

    #[inline]
    fn encode_adapter(&self, e: &mut Enc) {
        for &w in &self.w {
            e.i32(w);
        }
    }

    #[inline]
    fn encode_optimizer(&self, e: &mut Enc) {
        for &a in &self.acc {
            e.i64(a);
        }
        e.u64(self.t);
        e.u32(self.epochs);
    }

    /// The adapter's bytes: `w`, each `i32` little-endian.
    #[inline]
    #[must_use]
    pub fn adapter_bytes(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(ADAPTER_LEN);
        self.encode_adapter(&mut e);
        e.finish()
    }

    /// The optimizer's bytes: `acc`, each `i64` little-endian, then `t` and `epochs`.
    #[inline]
    #[must_use]
    pub fn optimizer_bytes(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(OPTIMIZER_LEN);
        self.encode_optimizer(&mut e);
        e.finish()
    }

    /// The random state's bytes.
    #[inline]
    #[must_use]
    pub const fn rng_bytes(&self) -> [u8; RNG_LEN] {
        self.rng.to_le_bytes()
    }

    /// Its [`STATE_LEN`] bytes.
    #[inline]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(STATE_LEN);
        e.raw(&STATE_MAGIC);
        e.u32(STATE_FORMAT);
        e.u32(FEATURES as u32);
        self.encode_adapter(&mut e);
        self.encode_optimizer(&mut e);
        e.raw(&self.rng_bytes());
        e.finish()
    }

    /// The state `bytes` encode, canonically: `decode(b)?.encode() == b`.
    /// # Errors
    /// The first [`StateRefusal`] that applies.
    #[inline]
    pub fn decode(bytes: &[u8]) -> Result<State, StateRefusal> {
        if bytes.len() != STATE_LEN {
            return Err(StateRefusal::Length { len: bytes.len() });
        }
        let length = |_| StateRefusal::Length { len: bytes.len() };
        let mut d = Dec::new(bytes);
        if d.raw("magic", 4).map_err(length)? != STATE_MAGIC {
            return Err(StateRefusal::Magic);
        }
        let found = d.u32("format").map_err(length)?;
        if found != STATE_FORMAT {
            return Err(StateRefusal::Format { found });
        }
        let found = d.u32("features").map_err(length)?;
        if found as usize != FEATURES {
            return Err(StateRefusal::Features { found });
        }
        let mut s = State::base();
        for w in &mut s.w {
            *w = d.i32("w").map_err(length)?;
        }
        for a in &mut s.acc {
            *a = d.i64("acc").map_err(length)?;
        }
        s.t = d.u64("t").map_err(length)?;
        s.epochs = d.u32("epochs").map_err(length)?;
        s.rng = d.u64("rng").map_err(length)?;
        d.finish().map_err(length)?;
        let t = i64::try_from(s.t).map_err(|_| StateRefusal::Bounds)?;
        let sum = t.saturating_mul(t.saturating_add(1)) / 2;
        if s.t > MAX_STEPS {
            return Err(StateRefusal::Bounds);
        }
        for &w in &s.w {
            if i64::from(w).abs() > t {
                return Err(StateRefusal::Bounds);
            }
        }
        for &a in &s.acc {
            if a.saturating_abs() > sum {
                return Err(StateRefusal::Bounds);
            }
        }
        Ok(s)
    }

    /// `plan`'s score, `Σ_{f ∈ φ(plan)} (t·B[f] + acc[f])`. The bounds the trainer and
    /// [`Self::decode`] keep put it below `2⁵⁰` in magnitude.
    #[inline]
    #[must_use]
    pub const fn score(&self, plan: &SchedulePlan) -> i64 {
        let t = if self.t > i64::MAX as u64 {
            i64::MAX
        } else {
            self.t as i64
        };
        let fs = features(plan);
        (t * (BASE[fs[0]] as i64) + self.acc[fs[0]])
            + (t * (BASE[fs[1]] as i64) + self.acc[fs[1]])
            + (t * (BASE[fs[2]] as i64) + self.acc[fs[2]])
    }

    #[inline]
    const fn w_sum(&self, fs: &[usize; 3]) -> i64 {
        (self.w[fs[0]] as i64) + (self.w[fs[1]] as i64) + (self.w[fs[2]] as i64)
    }
}

/// What orders a pool (design §13.2, §13.9): a version's state, the control, or a fixed
/// comparator of the report.
#[derive(Clone, Copy, Debug)]
pub enum Ranker<'s> {
    /// Score descending, then the tie key (`m6-rank v1`).
    Model(&'s State),
    /// The control's key, then the tie key (`m6-control v1`).
    Control,
    /// `NextStep` or `Cadence` first, then the tie key (`m6-w1 v1`).
    W1,
    /// `SettleAtStep ∧ NextStep` first, then W1's order (`m6-w2 v1`).
    W2,
}

impl Ranker<'_> {
    #[inline]
    fn first(&self, pool: &PoolId, plan: &SchedulePlan) -> i128 {
        match self {
            Ranker::Model(state) => -(state.score(plan) as i128),
            Ranker::Control => control_key(pool, plan) as i128,
            Ranker::W1 => w1_class(plan) as i128,
            Ranker::W2 => w2_class(plan) as i128,
        }
    }

    /// `plans`, pool `pool`'s, in its order: its key, then the tie key ascending, then (a tie
    /// key collision, which no pinned pool has) the plan's bytes.
    #[inline]
    #[must_use]
    pub fn order(&self, pool: &PoolId, plans: &[SchedulePlan]) -> Vec<SchedulePlan> {
        let mut keyed = Vec::with_capacity(plans.len());
        for p in plans {
            keyed.push(((self.first(pool, p), tie_key(pool, p), p.encode()), *p));
        }
        keyed.sort_unstable_by_key(|k| k.0);
        let mut out = Vec::with_capacity(keyed.len());
        for (_, p) in keyed {
            out.push(p);
        }
        out
    }

    /// The first [`JUDGED`] plans of its order of `plans`.
    /// # Errors
    /// [`LearnRefusal::PoolTooSmall`] for a pool of fewer than [`JUDGED`] plans.
    #[inline]
    pub fn top_k(
        &self,
        pool: &PoolId,
        plans: &[SchedulePlan],
    ) -> Result<[SchedulePlan; JUDGED], LearnRefusal> {
        let order = self.order(pool, plans);
        <[SchedulePlan; JUDGED]>::try_from(order.get(..JUDGED).unwrap_or(&[]))
            .map_err(|_| LearnRefusal::PoolTooSmall { pool: *pool })
    }
}

/// A label's place in the label order: admitted before refused, admitted plans by training
/// `(ΣT, ΣZ)` ascending, refused plans tied.
#[inline]
const fn label_order(label: &TrainLabel) -> (u8, u64, u64) {
    match label.totals() {
        Some(t) => (0, t.ticks, t.wasted),
        None => (1, 0, 0),
    }
}

/// Trains one chain member from `parent`'s complete state on `data`, its labels in manifest
/// order (`m6-train v1`, design §13.2): [`EPOCHS`] epochs, each a Fisher–Yates permutation of the
/// manifest order (from the last index down, each swapped with `below(i + 1)`), then a step for
/// each consecutive pair of the permutation whose label order differs.
/// # Errors
/// [`LearnRefusal::StepBudget`] past [`MAX_STEPS`] steps.
#[inline]
pub fn train(parent: &State, data: &[&TrainLabel]) -> Result<State, LearnRefusal> {
    let mut s = parent.clone();
    let mut rng = SplitMix::new(s.rng);
    let mut feats = Vec::with_capacity(data.len());
    let mut orders = Vec::with_capacity(data.len());
    for l in data {
        feats.push(features(&l.plan()));
        orders.push(label_order(l));
    }
    let mut perm = Vec::with_capacity(data.len());
    for _ in 0..EPOCHS {
        perm.clear();
        for i in 0..data.len() {
            perm.push(i);
        }
        for i in (1..perm.len()).rev() {
            let j = usize::try_from(rng.below(i as u64 + 1)).unwrap_or(0);
            perm.swap(i, j);
        }
        for pair in perm.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (hi, lo) = match orders[a].cmp(&orders[b]) {
                Ordering::Less => (a, b),
                Ordering::Greater => (b, a),
                Ordering::Equal => continue,
            };
            if s.t >= MAX_STEPS {
                return Err(LearnRefusal::StepBudget);
            }
            if s.w_sum(&feats[hi]) <= s.w_sum(&feats[lo]) {
                for &f in &feats[hi] {
                    s.w[f] = s.w[f].checked_add(1).ok_or(LearnRefusal::StepBudget)?;
                }
                for &f in &feats[lo] {
                    s.w[f] = s.w[f].checked_sub(1).ok_or(LearnRefusal::StepBudget)?;
                }
            }
            s.t += 1;
            for (acc, &w) in s.acc.iter_mut().zip(&s.w) {
                *acc = acc
                    .checked_add(i64::from(w))
                    .ok_or(LearnRefusal::StepBudget)?;
            }
        }
    }
    s.epochs = s
        .epochs
        .checked_add(EPOCHS)
        .ok_or(LearnRefusal::StepBudget)?;
    s.rng = rng.state();
    Ok(s)
}

/// `C(n, k)`, exactly.
#[inline]
const fn choose(n: u32, k: u32) -> u128 {
    let min_k = if k < n { k } else { n };
    let rem = n - min_k;
    let k = if k < rem { k } else { rem };
    let mut c = 1_u128;
    let mut i = 0;
    while i < k {
        c = c * ((n - i) as u128) / ((i + 1) as u128);
        i += 1;
    }
    c
}

/// The one-sided sign test at level `1 / level` (design §13.4): `n ≥ 1` and
/// `level · Σ_{k=w}^{n} C(n, k) ≤ 2ⁿ`, exactly in `u128`, for `n` up to [`POOLS`].
#[inline]
#[must_use]
pub const fn sign_test(level: u128, wins: u32, n: u32) -> bool {
    if n == 0 || wins > n || n as usize > POOLS {
        return false;
    }
    let mut tail = 0_u128;
    let mut k = wins;
    while k <= n {
        tail += choose(n, k);
        k += 1;
    }
    level * tail <= 1_u128 << n
}

/// Two rankers' outcomes compared pool by pool: wins, losses and summed gains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Pools the candidate's outcome is better in, by `(class, ΣT, ΣZ)`.
    pub wins: u32,
    /// Pools it is worse in.
    pub losses: u32,
    /// The candidate's summed `ΣT` and `ΣZ` gains.
    pub gain: (u64, u64),
    /// The comparator's.
    pub against: (u64, u64),
}

impl Comparison {
    /// `x`'s outcomes against `a`'s, pool by pool.
    #[inline]
    #[must_use]
    pub fn of(x: &[PoolOutcome], a: &[PoolOutcome]) -> Comparison {
        let mut c = Comparison::default();
        for (x, a) in x.iter().zip(a) {
            match x.key().cmp(&a.key()) {
                Ordering::Less => c.wins += 1,
                Ordering::Greater => c.losses += 1,
                Ordering::Equal => {}
            }
            let (gx, ga) = (x.gain(), a.gain());
            c.gain = (c.gain.0 + gx.0, c.gain.1 + gx.1);
            c.against = (c.against.0 + ga.0, c.against.1 + ga.1);
        }
        c
    }

    /// Whether the candidate beats the comparator at level `1 / level`: the sign test over the
    /// untied pools, and summed gains not lower in both `ΣT` and `ΣZ`.
    /// # Errors
    /// [`ModelRefusal::NoUntied`], [`ModelRefusal::Losses`] or [`ModelRefusal::Gain`].
    #[inline]
    pub const fn beats(&self, level: u128) -> Result<(), ModelRefusal> {
        let n = self.wins + self.losses;
        if n == 0 {
            Err(ModelRefusal::NoUntied)
        } else if !sign_test(level, self.wins, n) {
            Err(ModelRefusal::Losses)
        } else if self.gain.0 < self.against.0 || self.gain.1 < self.against.1 {
            Err(ModelRefusal::Gain)
        } else {
            Ok(())
        }
    }

    /// A look's verdict: [`Self::beats`] at [`LOOK_LEVEL`].
    /// # Errors
    /// As [`Self::beats`].
    #[inline]
    pub const fn admits(&self) -> Result<(), ModelRefusal> {
        self.beats(LOOK_LEVEL)
    }

    /// A report criterion's verdict: [`Self::beats`] at [`REPORT_LEVEL`].
    /// # Errors
    /// As [`Self::beats`].
    #[inline]
    pub const fn shows(&self) -> Result<(), ModelRefusal> {
        self.beats(REPORT_LEVEL)
    }
}

/// The domain of the digest of a manifest's candidate ids, in manifest order.
pub const IDS_DOMAIN: &[u8] = b"aterm-aos-m6-ids-v1\0";

/// The domain of a manifest's digest.
pub const MANIFEST_DOMAIN: &[u8] = b"aterm-aos-m6-manifest-v1\0";

/// The domain of one label's digest.
pub const LABEL_DOMAIN: &[u8] = b"aterm-aos-m6-label-v1\0";

/// The domain of a version's label digest, over its labels' digests in manifest order.
pub const LABELS_DOMAIN: &[u8] = b"aterm-aos-m6-labels-v1\0";

/// Where one manifest block's plans come from (design §13.3): a closed set, decoded canonically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// [`GRID_V1`]'s 54 plans, in order.
    Grid,
    /// Pool `pool` of the train set, in draw order.
    TrainPool {
        /// The pool.
        pool: u32,
    },
    /// Round `round`'s evidence on train pool `pool` (`2 + round`): `parent`'s top [`JUDGED`]
    /// there, then the [`EXPLORE`] other plans of the pool with the lowest tie keys.
    ChainEvidence {
        /// The round, 2 to [`ROUNDS`].
        round: u8,
        /// The parent's id.
        parent: Digest,
        /// The pool.
        pool: u32,
    },
}

/// Why manifest bytes are not a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestRefusal {
    /// The bytes do not decode.
    Wire(WireFault),
    /// A block's source tag no manifest writes.
    Source {
        /// The tag.
        tag: u8,
    },
    /// A plan does not decode, or its bytes are not its encoding.
    Plan,
    /// A candidate id appears twice.
    Duplicate,
    /// The recorded count or id digest is not the blocks'.
    Summary,
    /// The plans it records as dropped pass [`MAX_LABELS`]: no chain offers that many.
    Bounds,
}

impl fmt::Display for ManifestRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestRefusal::Wire(e) => write!(f, "manifest refused: {e}"),
            ManifestRefusal::Source { tag } => {
                write!(f, "manifest refused: source tag {tag} names no source")
            }
            ManifestRefusal::Plan => write!(f, "manifest refused: a plan does not decode"),
            ManifestRefusal::Duplicate => {
                write!(f, "manifest refused: a candidate id appears twice")
            }
            ManifestRefusal::Summary => write!(
                f,
                "manifest refused: its count or id digest is not its blocks'"
            ),
            ManifestRefusal::Bounds => {
                write!(f, "manifest refused: its dropped plans pass {MAX_LABELS}")
            }
        }
    }
}

impl std::error::Error for ManifestRefusal {}

/// A version's data selection (design §13.3): blocks of plans by source, deduplicated by
/// candidate id keeping the first occurrence, with the count of plans dropped as duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    blocks: Vec<(Source, Vec<SchedulePlan>)>,
    excluded: u32,
}

impl Manifest {
    /// The blocks, in order.
    #[inline]
    #[must_use]
    pub fn blocks(&self) -> &[(Source, Vec<SchedulePlan>)] {
        &self.blocks
    }

    /// The plans, in manifest order.
    #[inline]
    pub fn plans(&self) -> impl Iterator<Item = &SchedulePlan> {
        self.blocks.iter().flat_map(|(_, plans)| plans)
    }

    /// Its plan count.
    #[inline]
    #[must_use]
    pub fn count(&self) -> usize {
        let mut total = 0usize;
        for (_, plans) in &self.blocks {
            total += plans.len();
        }
        total
    }

    /// The plans dropped as duplicates.
    #[inline]
    #[must_use]
    pub const fn excluded(&self) -> u32 {
        self.excluded
    }

    /// The digest of its candidate ids, in manifest order.
    #[inline]
    #[must_use]
    pub fn ids_digest(&self) -> Digest {
        let n = self.count();
        let mut ids = Vec::with_capacity(n);
        for (_, plans) in &self.blocks {
            for p in plans {
                ids.push(p.digest());
            }
        }
        let mut parts = Vec::with_capacity(ids.len());
        for d in &ids {
            parts.push(d.0.as_slice());
        }
        Digest::of(IDS_DOMAIN, &parts)
    }

    /// This manifest with `source`'s `plans` appended, each plan whose id it holds dropped.
    #[inline]
    fn with(mut self, source: Source, plans: &[SchedulePlan]) -> Manifest {
        let mut taken = BTreeSet::new();
        for (_, block_plans) in &self.blocks {
            for p in block_plans {
                taken.insert(p.encode());
            }
        }
        let mut kept = Vec::with_capacity(plans.len());
        for plan in plans {
            if taken.insert(plan.encode()) {
                kept.push(*plan);
            } else {
                self.excluded += 1;
            }
        }
        self.blocks.push((source, kept));
        self
    }

    /// Its bytes: the block count, each block's source and plans, then the plan count, the id
    /// digest and the excluded count.
    #[inline]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.u64(self.blocks.len() as u64);
        for (source, plans) in &self.blocks {
            match source {
                Source::Grid => e.u8(0),
                Source::TrainPool { pool } => {
                    e.u8(1);
                    e.u32(*pool);
                }
                Source::ChainEvidence {
                    round,
                    parent,
                    pool,
                } => {
                    e.u8(2);
                    e.u8(*round);
                    e.digest(parent);
                    e.u32(*pool);
                }
            }
            e.u64(plans.len() as u64);
            for plan in plans {
                e.raw(&plan.encode());
            }
        }
        e.u64(self.count() as u64);
        e.digest(&self.ids_digest());
        e.u32(self.excluded);
        e.finish()
    }

    /// The manifest `bytes` encode, canonically.
    /// # Errors
    /// The first [`ManifestRefusal`] that applies.
    #[inline]
    pub fn decode(bytes: &[u8]) -> Result<Manifest, ManifestRefusal> {
        let mut d = Dec::new(bytes);
        let n = d
            .count("blocks", MAX_LABELS as u64)
            .map_err(ManifestRefusal::Wire)?;
        let mut m = Manifest::default();
        let mut seen = BTreeSet::new();
        for _ in 0..n {
            let source = match d.u8("source").map_err(ManifestRefusal::Wire)? {
                0 => Source::Grid,
                1 => Source::TrainPool {
                    pool: d.u32("pool").map_err(ManifestRefusal::Wire)?,
                },
                2 => Source::ChainEvidence {
                    round: d.u8("round").map_err(ManifestRefusal::Wire)?,
                    parent: d.digest("parent").map_err(ManifestRefusal::Wire)?,
                    pool: d.u32("pool").map_err(ManifestRefusal::Wire)?,
                },
                tag => return Err(ManifestRefusal::Source { tag }),
            };
            let k = d
                .count("plans", MAX_LABELS as u64)
                .map_err(ManifestRefusal::Wire)?;
            let mut plans = Vec::with_capacity(k);
            for _ in 0..k {
                let raw = d.raw("plan", 12).map_err(ManifestRefusal::Wire)?;
                let plan = SchedulePlan::decode(raw).map_err(|_| ManifestRefusal::Plan)?;
                if plan.encode() != raw {
                    return Err(ManifestRefusal::Plan);
                }
                if !seen.insert(plan.encode()) {
                    return Err(ManifestRefusal::Duplicate);
                }
                plans.push(plan);
            }
            m.blocks.push((source, plans));
        }
        let count = d.u64("count").map_err(ManifestRefusal::Wire)?;
        let ids = d.digest("ids").map_err(ManifestRefusal::Wire)?;
        m.excluded = d.u32("excluded").map_err(ManifestRefusal::Wire)?;
        d.finish().map_err(ManifestRefusal::Wire)?;
        if count != m.count() as u64 || ids != m.ids_digest() {
            return Err(ManifestRefusal::Summary);
        }
        if u64::from(m.excluded) > MAX_LABELS as u64 {
            return Err(ManifestRefusal::Bounds);
        }
        Ok(m)
    }

    /// The digest of its bytes.
    #[inline]
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(MANIFEST_DOMAIN, &[&self.encode()])
    }
}

/// `n` as a `u32`, saturating (every count here is far below).
#[inline]
const fn len32(n: usize) -> u32 {
    if n > u32::MAX as usize {
        u32::MAX
    } else {
        n as u32
    }
}

/// One label's digest: its candidate id, then its training totals or its first failing law.
#[inline]
#[must_use]
pub fn label_digest(label: &TrainLabel) -> Digest {
    let mut e = Enc::new();
    e.digest(&label.id());
    match (label.totals(), label.law()) {
        (Some(t), _) => {
            e.u8(0);
            e.u64(t.ticks);
            e.u64(t.presented);
            e.u64(t.wasted);
        }
        (None, law) => {
            e.u8(1);
            e.str(law.unwrap_or(""));
        }
    }
    Digest::of(LABEL_DOMAIN, &[&e.finish()])
}

/// The label digest of labels whose digests are `each`, in manifest order.
#[inline]
#[must_use]
pub fn labels_digest(each: &[Digest]) -> Digest {
    let mut parts = Vec::with_capacity(each.len());
    for d in each {
        parts.push(d.0.as_slice());
    }
    Digest::of(LABELS_DOMAIN, &parts)
}

/// Why the learner refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LearnRefusal {
    /// A pool holds fewer than [`JUDGED`] plans.
    PoolTooSmall {
        /// The pool.
        pool: PoolId,
    },
    /// Training would take `t` past [`MAX_STEPS`].
    StepBudget,
    /// A version's data would pass [`MAX_LABELS`] plans.
    LabelBudget {
        /// Its count.
        count: usize,
    },
    /// No chain member past round [`ROUNDS`].
    Round {
        /// The round asked for.
        round: u8,
    },
    /// A pool did not fill.
    Pool(PoolRefusal),
    /// Labelling refused.
    Label(LabelRefusal),
}

impl fmt::Display for LearnRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LearnRefusal::PoolTooSmall { pool } => write!(
                f,
                "learner refused: pool {} of set {:?} {} holds fewer than {JUDGED} plans",
                pool.pool, pool.set.tag, pool.set.index
            ),
            LearnRefusal::StepBudget => {
                write!(f, "learner refused: StepBudget, past {MAX_STEPS} steps")
            }
            LearnRefusal::LabelBudget { count } => write!(
                f,
                "learner refused: LabelBudget, {count} plans past {MAX_LABELS}"
            ),
            LearnRefusal::Round { round } => {
                write!(f, "learner refused: no round {round}, past {ROUNDS}")
            }
            LearnRefusal::Pool(e) => write!(f, "learner refused: {e}"),
            LearnRefusal::Label(e) => write!(f, "learner refused: {e}"),
        }
    }
}

impl std::error::Error for LearnRefusal {}

impl From<PoolRefusal> for LearnRefusal {
    fn from(e: PoolRefusal) -> LearnRefusal {
        LearnRefusal::Pool(e)
    }
}

impl From<LabelRefusal> for LearnRefusal {
    fn from(e: LabelRefusal) -> LearnRefusal {
        LearnRefusal::Label(e)
    }
}

/// The train set.
pub const TRAIN: SetId = SetId {
    tag: SetTag::Train,
    index: 0,
};

/// How much of each set a [`Sets`] draws: all of it, or (tests) a prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scale {
    /// Plans of each train pool.
    train_plans: usize,
    /// Pools of each other set.
    pools: usize,
    /// Plans of each of its pools.
    plans: usize,
    /// Grid plans.
    grid: usize,
}

impl Scale {
    const FULL: Scale = Scale {
        train_plans: POOL_PLANS,
        pools: POOLS,
        plans: POOL_PLANS,
        grid: usize::MAX,
    };
}

/// The pools M6 reads (design §13.3), each drawn once: v1's sets in full, or, in tests, a prefix
/// of each set and of each of its pools (every plan still in its set's pools, as a token checks).
#[derive(Debug)]
pub struct Sets {
    scale: Scale,
    drawn: Mutex<BTreeMap<SetId, Vec<Vec<SchedulePlan>>>>,
}

impl Sets {
    /// v1's sets in full.
    #[inline]
    #[must_use]
    pub fn v1() -> Sets {
        Sets {
            scale: Scale::FULL,
            drawn: Mutex::new(BTreeMap::new()),
        }
    }

    /// A prefix of every set: `train_plans` plans of each train pool, `pools` pools of each other
    /// set with `plans` plans each, and `grid` grid plans (tests).
    #[cfg(test)]
    pub(super) fn scaled(train_plans: usize, pools: usize, plans: usize, grid: usize) -> Sets {
        Sets {
            scale: Scale {
                train_plans,
                pools,
                plans,
                grid,
            },
            drawn: Mutex::new(BTreeMap::new()),
        }
    }

    /// Whether it draws v1's sets in full.
    #[inline]
    #[must_use]
    pub const fn full(&self) -> bool {
        self.scale.train_plans == Scale::FULL.train_plans
            && self.scale.pools == Scale::FULL.pools
            && self.scale.plans == Scale::FULL.plans
            && self.scale.grid == Scale::FULL.grid
    }

    /// `set`'s pools, in pool order.
    /// # Errors
    /// The first pool that does not fill.
    #[inline]
    pub fn pools(&self, set: SetId) -> Result<Vec<Vec<SchedulePlan>>, PoolRefusal> {
        let mut drawn = self.drawn.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(pools) = drawn.get(&set) {
            return Ok(pools.clone());
        }
        let (pools, plans) = if set.tag == SetTag::Train {
            (usize::MAX, self.scale.train_plans)
        } else {
            (self.scale.pools, self.scale.plans)
        };
        let mut all = set_pools(set)?;
        all.truncate(pools);
        for pool in &mut all {
            pool.truncate(plans);
        }
        drawn.insert(set, all.clone());
        Ok(all)
    }

    /// [`GRID_V1`]'s plans, in order.
    #[inline]
    #[must_use]
    pub fn grid(&self) -> Vec<SchedulePlan> {
        let cap = self.scale.grid.min(GRID_V1.len());
        let mut out = Vec::with_capacity(cap);
        for &(split, wake) in &GRID_V1 {
            if out.len() >= self.scale.grid {
                break;
            }
            if let Ok(p) = SchedulePlan::new(split, wake) {
                out.push(p);
            }
        }
        out
    }
}

/// Labels plans through one evaluator and spread; the evaluator keeps every label for its life.
#[derive(Clone, Copy)]
pub struct Labeller<'e> {
    ev: &'e Evaluator,
    spread: &'e dyn Spread,
}

impl fmt::Debug for Labeller<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Labeller").finish_non_exhaustive()
    }
}

impl<'e> Labeller<'e> {
    /// Labels through `ev`, judging new plans through `spread`.
    #[inline]
    #[must_use]
    pub const fn new(ev: &'e Evaluator, spread: &'e dyn Spread) -> Labeller<'e> {
        Labeller { ev, spread }
    }

    /// `plans`' labels under `scope`, in order.
    /// # Errors
    /// The scope's refusal, before anything is judged.
    #[inline]
    pub fn label(
        &self,
        scope: LabelScope,
        plans: &[[u8; 12]],
    ) -> Result<Vec<TrainLabel>, LabelRefusal> {
        self.ev.label_spread(scope, plans, self.spread)
    }
}

/// The plans as bytes.
#[inline]
fn bytes_of<'p>(plans: impl IntoIterator<Item = &'p SchedulePlan>) -> Vec<[u8; 12]> {
    let iter = plans.into_iter();
    let mut out = Vec::with_capacity(iter.size_hint().0);
    for p in iter {
        out.push(p.encode());
    }
    out
}

/// Why a version's files are not a version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionRefusal {
    /// The descriptor does not decode.
    Descriptor(WireFault),
    /// The state does not decode.
    State(StateRefusal),
    /// The manifest does not decode.
    Manifest(ManifestRefusal),
    /// The labels file is not one digest per manifest plan.
    Labels,
    /// The files disagree with the descriptor's parts.
    Parts,
}

impl fmt::Display for VersionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionRefusal::Descriptor(e) => write!(f, "version refused: descriptor: {e}"),
            VersionRefusal::State(e) => write!(f, "version refused: {e}"),
            VersionRefusal::Manifest(e) => write!(f, "version refused: {e}"),
            VersionRefusal::Labels => write!(
                f,
                "version refused: its labels file is not one digest per manifest plan"
            ),
            VersionRefusal::Parts => write!(
                f,
                "version refused: its files are not the parts its descriptor names"
            ),
        }
    }
}

impl std::error::Error for VersionRefusal {}

/// One model version (design §13.2): its parent, round, state, data and labels, named by the
/// digest of exactly those parts, which names no build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    id: Digest,
    parent: Option<Digest>,
    round: u8,
    state: State,
    manifest: Manifest,
    labels: Vec<Digest>,
}

impl Version {
    /// v0: the base, round 0, no parent, no data.
    #[inline]
    #[must_use]
    pub fn base() -> Version {
        Version::of(None, 0, State::base(), Manifest::default(), Vec::new())
    }

    #[inline]
    fn of(
        parent: Option<Digest>,
        round: u8,
        state: State,
        manifest: Manifest,
        labels: Vec<Digest>,
    ) -> Version {
        let id = version_id(
            parent.as_ref(),
            round,
            &state,
            &manifest.digest(),
            &labels_digest(&labels),
        );
        Version {
            id,
            parent,
            round,
            state,
            manifest,
            labels,
        }
    }

    /// Its id.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> Digest {
        self.id
    }

    /// Its parent's id, `None` for v0.
    #[inline]
    #[must_use]
    pub const fn parent(&self) -> Option<Digest> {
        self.parent
    }

    /// Its round: 0 for v0, 1 to [`ROUNDS`] for the chain's members.
    #[inline]
    #[must_use]
    pub const fn round(&self) -> u8 {
        self.round
    }

    /// Its state.
    #[inline]
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// Its data selection.
    #[inline]
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Its labels' digests, in manifest order.
    #[inline]
    #[must_use]
    pub fn labels(&self) -> &[Digest] {
        &self.labels
    }

    /// Its label digest.
    #[inline]
    #[must_use]
    pub fn label_digest(&self) -> Digest {
        labels_digest(&self.labels)
    }

    /// The bytes of its labels file: each label's digest, in manifest order.
    #[inline]
    #[must_use]
    pub fn labels_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.labels.len() * 32);
        for d in &self.labels {
            out.extend_from_slice(&d.0);
        }
        out
    }

    /// Its descriptor: exactly the parts its id is the digest of, in order.
    #[inline]
    #[must_use]
    pub fn descriptor(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.digest(&M6_POLICY_V1_DIGEST);
        e.opt(self.parent.as_ref(), |e, p| e.digest(p));
        e.u8(self.round);
        e.digest(&base_digest());
        e.bytes(&self.state.adapter_bytes());
        e.bytes(&self.state.optimizer_bytes());
        e.raw(&self.state.rng_bytes());
        e.digest(&self.manifest.digest());
        e.digest(&self.label_digest());
        e.finish()
    }

    /// The version a store's files hold: its descriptor, state, manifest and labels file, each
    /// checked against the others. A round past [`ROUNDS`], a member without a parent, or a round
    /// 0 that is not v0 is refused as [`VersionRefusal::Parts`].
    /// # Errors
    /// The first [`VersionRefusal`] that applies.
    #[inline]
    pub fn from_files(
        descriptor: &[u8],
        state: &[u8],
        manifest: &[u8],
        labels: &[u8],
    ) -> Result<Version, VersionRefusal> {
        let state = State::decode(state).map_err(VersionRefusal::State)?;
        let manifest = Manifest::decode(manifest).map_err(VersionRefusal::Manifest)?;
        let (chunks, rest) = labels.as_chunks::<32>();
        if !rest.is_empty() || chunks.len() != manifest.count() {
            return Err(VersionRefusal::Labels);
        }
        let mut decoded_labels = Vec::with_capacity(chunks.len());
        for c in chunks {
            decoded_labels.push(Digest(*c));
        }
        let labels = decoded_labels;
        let mut d = Dec::new(descriptor);
        let wire = VersionRefusal::Descriptor;
        let policy = d.digest("policy").map_err(wire)?;
        let parent = d.opt("parent", |d| d.digest("parent")).map_err(wire)?;
        let round = d.u8("round").map_err(wire)?;
        let base = d.digest("base").map_err(wire)?;
        let adapter = d.bytes("adapter").map_err(wire)?;
        let optimizer = d.bytes("optimizer").map_err(wire)?;
        let rng = d.raw("rng", RNG_LEN).map_err(wire)?;
        let manifest_digest = d.digest("manifest").map_err(wire)?;
        let label_digest = d.digest("labels").map_err(wire)?;
        d.finish().map_err(wire)?;
        let version = Version::of(parent, round, state, manifest, labels);
        if policy != M6_POLICY_V1_DIGEST
            || base != base_digest()
            || adapter != version.state.adapter_bytes()
            || optimizer != version.state.optimizer_bytes()
            || rng != version.state.rng_bytes()
            || manifest_digest != version.manifest.digest()
            || label_digest != version.label_digest()
            || version.descriptor() != descriptor
            || version.round > ROUNDS
            || version.parent.is_none() != (version.round == 0)
            || (version.round == 0 && version != Version::base())
        {
            return Err(VersionRefusal::Parts);
        }
        Ok(version)
    }
}

/// One part of a version changed, for the negative controls (tests).
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) enum Alter {
    /// `w[0]` moved by this much.
    Weight(i32),
    /// `acc` zeroed.
    Average,
    /// `t` reset.
    Steps,
    /// `epochs` changed.
    Epochs,
    /// The random state reseeded.
    Random,
    /// The manifest's first plan dropped, with its label.
    DropPlan,
    /// The first label's digest altered.
    Label,
    /// The round set.
    Round(u8),
}

#[cfg(test)]
impl Version {
    /// This version with one part changed, named by the digest of its parts as any version is.
    pub(super) fn altered(&self, how: Alter) -> Version {
        let (mut state, mut manifest, mut labels) = (
            self.state.clone(),
            self.manifest.clone(),
            self.labels.clone(),
        );
        match how {
            Alter::Weight(by) => state.w[0] += by,
            Alter::Average => state.acc = [0; FEATURES],
            Alter::Steps => state.t = 0,
            Alter::Epochs => state.epochs += 1,
            Alter::Random => state.rng = SplitMix::new(state.rng ^ 1).state(),
            Alter::DropPlan => {
                if let Some((_, plans)) = manifest.blocks.first_mut() {
                    plans.remove(0);
                }
                labels.remove(0);
            }
            Alter::Label => labels[0].0[0] ^= 1,
            Alter::Round(round) => {
                return Version::of(self.parent, round, state, manifest, labels);
            }
        }
        Version::of(self.parent, self.round, state, manifest, labels)
    }
}

/// A version's id: `Digest::of` its parts under [`VERSION_DOMAIN`] (design §13.2).
#[inline]
fn version_id(
    parent: Option<&Digest>,
    round: u8,
    state: &State,
    manifest: &Digest,
    labels: &Digest,
) -> Digest {
    Digest::of(
        VERSION_DOMAIN,
        &[
            &M6_POLICY_V1_DIGEST.0,
            parent.map_or(&[][..], |p| &p.0[..]),
            &[round],
            &base_digest().0,
            &state.adapter_bytes(),
            &state.optimizer_bytes(),
            &state.rng_bytes(),
            &manifest.0,
            &labels.0,
        ],
    )
}

/// Round `round`'s evidence on train pool `2 + round`: `parent`'s top [`JUDGED`] there, then the
/// [`EXPLORE`] other plans with the lowest tie keys.
#[inline]
fn evidence(
    parent: &Version,
    round: u8,
    sets: &Sets,
) -> Result<(Source, Vec<SchedulePlan>), LearnRefusal> {
    let pool = 2 + u32::from(round);
    let plans = sets
        .pools(TRAIN)?
        .into_iter()
        .nth(pool as usize)
        .unwrap_or_default();
    let id = PoolId { set: TRAIN, pool };
    let top = Ranker::Model(&parent.state).top_k(&id, &plans)?;
    let mut rest = Vec::with_capacity(plans.len());
    for p in plans {
        if !top.contains(&p) {
            rest.push(p);
        }
    }
    rest.sort_unstable_by_key(|p| (tie_key(&id, p), p.encode()));
    rest.truncate(EXPLORE);
    let mut out = top.to_vec();
    out.extend(rest);
    Ok((
        Source::ChainEvidence {
            round,
            parent: parent.id,
            pool,
        },
        out,
    ))
}

/// The round of the member that trains from `parent`: its round + 1, at most [`ROUNDS`].
#[inline]
const fn next_round(parent: &Version) -> Result<u8, LearnRefusal> {
    match parent.round.checked_add(1) {
        Some(round) if round <= ROUNDS => Ok(round),
        _ => Err(LearnRefusal::Round {
            round: parent.round.saturating_add(1),
        }),
    }
}

/// The data selection of the member that trains from `parent` (`m6-data v1`, design §13.3).
/// # Errors
/// [`LearnRefusal::Round`] past [`ROUNDS`], or a pool's refusal.
#[inline]
pub fn data(parent: &Version, sets: &Sets) -> Result<Manifest, LearnRefusal> {
    let round = next_round(parent)?;
    if round == 1 {
        let pools = sets.pools(TRAIN)?;
        let mut m = Manifest::default().with(Source::Grid, &sets.grid());
        for p in 0..4_u32 {
            let plans = pools.get(p as usize).cloned().unwrap_or_default();
            m = m.with(Source::TrainPool { pool: p }, &plans);
        }
        return Ok(m);
    }
    let (source, plans) = evidence(parent, round, sets)?;
    Ok(parent.manifest.clone().with(source, &plans))
}

/// Whether a version's data of `count` plans is within [`MAX_LABELS`].
/// # Errors
/// [`LearnRefusal::LabelBudget`] past it.
#[inline]
pub const fn label_budget(count: usize) -> Result<(), LearnRefusal> {
    if count > MAX_LABELS {
        Err(LearnRefusal::LabelBudget { count })
    } else {
        Ok(())
    }
}

/// The chain member that trains from `parent`: its data, labelled on the training split, and one
/// training run from `parent`'s state (design §13.2).
/// # Errors
/// [`LearnRefusal::Round`], [`LearnRefusal::LabelBudget`], a pool's or labelling's refusal, or
/// [`LearnRefusal::StepBudget`].
#[inline]
pub fn derive(
    parent: &Version,
    sets: &Sets,
    labeller: &Labeller<'_>,
) -> Result<Version, LearnRefusal> {
    let round = next_round(parent)?;
    let manifest = data(parent, sets)?;
    label_budget(manifest.count())?;
    let labels = labeller.label(LabelScope::Train, &bytes_of(manifest.plans()))?;
    let mut refs = Vec::with_capacity(labels.len());
    let mut digests = Vec::with_capacity(labels.len());
    for l in &labels {
        refs.push(l);
        digests.push(label_digest(l));
    }
    let state = train(&parent.state, &refs)?;
    Ok(Version::of(
        Some(parent.id),
        round,
        state,
        manifest,
        digests,
    ))
}

/// The chain v0..v4, computed on training data alone (design §13.2).
/// # Errors
/// The first member's refusal.
#[inline]
pub fn chain(sets: &Sets, labeller: &Labeller<'_>) -> Result<Vec<Version>, LearnRefusal> {
    let mut last = Version::base();
    let mut members = vec![last.clone()];
    for _ in 0..ROUNDS {
        last = derive(&last, sets, labeller)?;
        members.push(last.clone());
    }
    Ok(members)
}

/// The domain of a chain's digest.
pub const CHAIN_DOMAIN: &[u8] = b"aterm-aos-m6-chain-v1\0";

/// The chain's members v0..v4, by id and label digest: what [`M6_CHAIN_V1`] pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainPin {
    /// The members' ids, v0 first.
    pub ids: [Digest; 1 + ROUNDS as usize],
    /// Their label digests.
    pub labels: [Digest; 1 + ROUNDS as usize],
}

impl ChainPin {
    /// The pin of `members`, v0..v4.
    #[inline]
    #[must_use]
    pub fn of(members: &[Version]) -> Option<ChainPin> {
        const LEN: usize = 1 + ROUNDS as usize;
        if members.len() != LEN {
            return None;
        }
        let mut ids = [Digest([0; 32]); LEN];
        let mut labels = [Digest([0; 32]); LEN];
        for (i, m) in members.iter().enumerate() {
            ids[i] = m.id();
            labels[i] = m.label_digest();
        }
        Some(ChainPin { ids, labels })
    }

    /// Its digest, which every ledger's `open` line records.
    #[inline]
    #[must_use]
    pub fn digest(&self) -> Digest {
        const LEN: usize = (1 + ROUNDS as usize) * 2;
        let mut parts: [&[u8]; LEN] = [&[]; LEN];
        let n = 1 + ROUNDS as usize;
        let mut i = 0;
        while i < n {
            parts[i] = &self.ids[i].0;
            parts[n + i] = &self.labels[i].0;
            i += 1;
        }
        Digest::of(CHAIN_DOMAIN, &parts)
    }
}

/// The v1 chain, pinned on training data alone before the rehearsal (design §13.7, stage E) from
/// `aos-learn chain` at aterm-lab 0.145.0: digest
/// `f3ae385050abe4b23912ec63291af550d6445337e3c72adee7f0645a3113afa7`.
pub const M6_CHAIN_V1: Option<ChainPin> = Some(ChainPin {
    ids: [
        Digest([
            0x82, 0x8d, 0xbc, 0x65, 0x3f, 0x93, 0x3c, 0x25, 0x2e, 0x1f, 0xd2, 0xee, 0x54, 0xc0,
            0xab, 0xf6, 0x70, 0x0a, 0x16, 0x9e, 0xe2, 0x59, 0xa6, 0xe7, 0x6f, 0xd4, 0xf3, 0x5b,
            0x8a, 0x1d, 0x36, 0x54,
        ]),
        Digest([
            0x1d, 0xed, 0x71, 0x63, 0xa4, 0x11, 0x03, 0x70, 0xec, 0x6d, 0xa1, 0x92, 0xdb, 0xd5,
            0x7f, 0xa0, 0xc7, 0x35, 0x0c, 0x7c, 0x7f, 0x07, 0x96, 0x38, 0xcc, 0xd0, 0x7f, 0x88,
            0x2e, 0x15, 0xd7, 0x1c,
        ]),
        Digest([
            0xf3, 0x27, 0x30, 0x6a, 0xa0, 0x77, 0x40, 0x6d, 0x1c, 0x90, 0x28, 0xfe, 0x88, 0x47,
            0xaf, 0x21, 0x3c, 0x05, 0x15, 0x17, 0xab, 0xe4, 0xad, 0x15, 0xbb, 0xd3, 0x0c, 0x81,
            0xfe, 0x26, 0x56, 0x41,
        ]),
        Digest([
            0x14, 0x84, 0x84, 0xf2, 0xbb, 0xbc, 0x43, 0x2a, 0xc5, 0xb1, 0x28, 0x06, 0x7d, 0x97,
            0xe0, 0x01, 0x76, 0xfa, 0x30, 0x6b, 0x8b, 0xbc, 0x5d, 0x71, 0xf7, 0x12, 0x3c, 0xbd,
            0xeb, 0xbd, 0xae, 0xcb,
        ]),
        Digest([
            0x45, 0x50, 0xd0, 0x0f, 0x57, 0x84, 0x3e, 0x96, 0xf4, 0x85, 0x60, 0x11, 0xaa, 0x5b,
            0xe6, 0xd7, 0x90, 0x36, 0xe2, 0x42, 0x0e, 0x06, 0xb5, 0x74, 0x86, 0x89, 0xf7, 0x96,
            0x25, 0xfc, 0x61, 0x80,
        ]),
    ],
    labels: [
        Digest([
            0xf8, 0x5a, 0x73, 0x03, 0xc4, 0x47, 0xb9, 0xfb, 0xf9, 0x18, 0x84, 0x63, 0x0e, 0x42,
            0x9e, 0x3e, 0xc5, 0x28, 0x98, 0x66, 0xd0, 0x05, 0x28, 0x16, 0x1d, 0x0b, 0x05, 0x59,
            0x5a, 0x98, 0x56, 0x1c,
        ]),
        Digest([
            0xd5, 0xb7, 0x9f, 0x20, 0x7e, 0x6b, 0x60, 0x7d, 0xe8, 0xdb, 0xa3, 0x38, 0x85, 0xbc,
            0x4a, 0x2d, 0x98, 0x65, 0x26, 0xa4, 0xa7, 0x93, 0x67, 0xf1, 0x04, 0x61, 0x8e, 0x92,
            0x26, 0xc0, 0x4c, 0xe8,
        ]),
        Digest([
            0x6d, 0x66, 0x9a, 0xfe, 0x35, 0x08, 0xa6, 0x21, 0x5d, 0x6b, 0x5f, 0xb7, 0x47, 0x78,
            0x8c, 0x33, 0x63, 0xc8, 0x80, 0xd3, 0x0a, 0x67, 0x9f, 0x7f, 0xc2, 0x15, 0xc1, 0xcb,
            0x64, 0x7e, 0x8f, 0xd1,
        ]),
        Digest([
            0x4b, 0xa2, 0x5b, 0x55, 0xde, 0xcc, 0x57, 0x6f, 0x9a, 0xad, 0x98, 0xa1, 0xf5, 0x7e,
            0x16, 0xe1, 0x17, 0xac, 0x50, 0x8a, 0x19, 0x06, 0x39, 0x6b, 0xb0, 0xcf, 0x67, 0x3d,
            0xa3, 0x2c, 0xda, 0xf4,
        ]),
        Digest([
            0x3a, 0x5f, 0xb4, 0x77, 0xc5, 0x46, 0x49, 0xf6, 0x4a, 0x33, 0x1f, 0x97, 0xbc, 0xeb,
            0xf4, 0xe6, 0x24, 0x47, 0x5a, 0x34, 0x32, 0xbb, 0xf9, 0x79, 0x36, 0x98, 0xeb, 0x22,
            0xc4, 0xe1, 0x40, 0x25,
        ]),
    ],
});

/// The domain of a ledger line's link, which chains each line to the one before.
pub const LEDGER_DOMAIN: &[u8] = b"aterm-aos-m6-ledger-v1\0";

/// A pair of sets one session reads: real pair `i` (`H_i` and `E_i`) or practice pair `j`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pair {
    /// `H_i` and `E_i`, `i` in 1..=3.
    Real(u32),
    /// Practice-held-out `j` and practice-report `j`.
    Practice(u32),
}

impl Pair {
    /// The set its looks read.
    #[inline]
    #[must_use]
    pub const fn looks(self) -> SetId {
        match self {
            Pair::Real(index) => SetId {
                tag: SetTag::HeldOut,
                index,
            },
            Pair::Practice(index) => SetId {
                tag: SetTag::PracticeHeldOut,
                index,
            },
        }
    }

    /// The set its report reads.
    #[inline]
    #[must_use]
    pub const fn report(self) -> SetId {
        match self {
            Pair::Real(index) => SetId {
                tag: SetTag::Report,
                index,
            },
            Pair::Practice(index) => SetId {
                tag: SetTag::PracticeReport,
                index,
            },
        }
    }

    /// Its ledger's directory name under the version's root: `pair-<i>` or `practice-<j>`.
    #[inline]
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Pair::Real(i) => format!("pair-{i}"),
            Pair::Practice(j) => format!("practice-{j}"),
        }
    }

    #[inline]
    const fn parts(self) -> (u8, u32) {
        match self {
            Pair::Real(i) => (0, i),
            Pair::Practice(j) => (1, j),
        }
    }
}

/// What a void ends, by when it arose (design §13.4, rule 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoidClass {
    /// Before the pair's first `look-begin`: nothing held out was read, and the pair may be opened
    /// again.
    Stopped,
    /// After the first `look-begin` and before its verdict, decided by the runner itself: the next
    /// pair may be opened.
    Opening,
    /// Any other: the policy version is closed.
    Closing,
}

/// What the runner decided a void on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoidCause {
    /// Its watcher killed a child at the deadline.
    Deadline,
    /// A child exited by panic or with an error.
    ChildFailed,
    /// A gate decided after the children exited: G2's replay, G3's guard or G4's footprint.
    Gate,
    /// Anything else: an operator's kill, a signal the watcher did not send, a kill on an
    /// interrupt, or a stop before any look.
    Other,
}

/// The class of a void decided on `cause` after `lines` (design §13.4, rule 6).
#[inline]
#[must_use]
pub const fn void_class(lines: &[Line], cause: VoidCause) -> VoidClass {
    let start = match last_open(lines) {
        Some(at) => at,
        None => 0,
    };
    let session = lines.split_at(start).1;
    let mut begun = false;
    let mut decided = false;
    let mut i = 0;
    while i < session.len() {
        match session[i] {
            Line::LookBegin { .. } => begun = true,
            Line::LookVerdict { .. } => decided = true,
            _ => {}
        }
        i += 1;
    }
    match (begun, decided, cause) {
        (false, _, _) => VoidClass::Stopped,
        (true, false, VoidCause::Deadline | VoidCause::ChildFailed) => VoidClass::Opening,
        _ => VoidClass::Closing,
    }
}

/// One ledger line (design §13.4, rule 4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A session opened on a store.
    Open {
        /// The store's id.
        store: Digest,
        /// The build.
        build: String,
        /// The chain's digest ([`ChainPin::digest`]).
        chain: Digest,
        /// The digest of the tests log the operator ran on the session commit.
        tests_log: Digest,
        /// A replaying child's open, after `retire`.
        replay: bool,
    },
    /// A look, its candidate and comparator by id, and its plan list's digest.
    LookBegin {
        /// 1 to [`LOOKS`].
        look: u8,
        /// The candidate.
        candidate: Digest,
        /// The comparator.
        comparator: Digest,
        /// The list's digest.
        list: Digest,
    },
    /// The look's bit.
    LookVerdict {
        /// Admitted.
        bit: bool,
    },
    /// The report's read, and its plan list's digest.
    ReportBegin {
        /// The list's digest.
        list: Digest,
    },
    /// The report written, by its digest.
    ReportDone {
        /// The report file's digest.
        report: Digest,
    },
    /// A look replayed, which spends nothing.
    Replay {
        /// The look.
        look: u8,
    },
    /// The admission set retired.
    Retire,
    /// A void, by its cause and class.
    Void {
        /// The cause.
        cause: VoidCause,
        /// The class.
        class: VoidClass,
    },
    /// A finished session of record.
    Record,
}

impl Line {
    #[inline]
    const fn kind(&self) -> u8 {
        match self {
            Line::Open { .. } => 0,
            Line::LookBegin { .. } => 1,
            Line::LookVerdict { .. } => 2,
            Line::ReportBegin { .. } => 3,
            Line::ReportDone { .. } => 4,
            Line::Replay { .. } => 5,
            Line::Retire => 6,
            Line::Void { .. } => 7,
            Line::Record => 8,
        }
    }

    /// Line `k`'s file bytes, chained to `prev`.
    #[inline]
    fn encode(&self, k: u64, prev: &Digest) -> Vec<u8> {
        let mut e = Enc::new();
        e.u64(k);
        e.digest(prev);
        e.u8(self.kind());
        match self {
            Line::Open {
                store,
                build,
                chain,
                tests_log,
                replay,
            } => {
                e.digest(store);
                e.str(build);
                e.digest(chain);
                e.digest(tests_log);
                e.bool(*replay);
            }
            Line::LookBegin {
                look,
                candidate,
                comparator,
                list,
            } => {
                e.u8(*look);
                e.digest(candidate);
                e.digest(comparator);
                e.digest(list);
            }
            Line::LookVerdict { bit } => e.bool(*bit),
            Line::ReportBegin { list } => e.digest(list),
            Line::ReportDone { report } => e.digest(report),
            Line::Replay { look } => e.u8(*look),
            Line::Retire | Line::Record => {}
            Line::Void { cause, class } => {
                e.u8(match cause {
                    VoidCause::Deadline => 0,
                    VoidCause::ChildFailed => 1,
                    VoidCause::Gate => 2,
                    VoidCause::Other => 3,
                });
                e.u8(match class {
                    VoidClass::Stopped => 0,
                    VoidClass::Opening => 1,
                    VoidClass::Closing => 2,
                });
            }
        }
        e.finish()
    }

    /// The line `bytes` hold, if they are line `k` chained to `prev`.
    #[inline]
    fn decode(bytes: &[u8], k: u64, prev: &Digest) -> Result<Line, LedgerRefusal> {
        let broken = |_| LedgerRefusal::Broken { line: k };
        let mut d = Dec::new(bytes);
        if d.u64("k").map_err(broken)? != k || d.digest("prev").map_err(broken)? != *prev {
            return Err(LedgerRefusal::Broken { line: k });
        }
        let line = match d.u8("kind").map_err(broken)? {
            0 => Line::Open {
                store: d.digest("store").map_err(broken)?,
                build: d.str("build").map_err(broken)?.to_owned(),
                chain: d.digest("chain").map_err(broken)?,
                tests_log: d.digest("tests_log").map_err(broken)?,
                replay: d.bool("replay").map_err(broken)?,
            },
            1 => Line::LookBegin {
                look: d.u8("look").map_err(broken)?,
                candidate: d.digest("candidate").map_err(broken)?,
                comparator: d.digest("comparator").map_err(broken)?,
                list: d.digest("list").map_err(broken)?,
            },
            2 => Line::LookVerdict {
                bit: d.bool("bit").map_err(broken)?,
            },
            3 => Line::ReportBegin {
                list: d.digest("list").map_err(broken)?,
            },
            4 => Line::ReportDone {
                report: d.digest("report").map_err(broken)?,
            },
            5 => Line::Replay {
                look: d.u8("look").map_err(broken)?,
            },
            6 => Line::Retire,
            7 => Line::Void {
                cause: match d.u8("cause").map_err(broken)? {
                    0 => VoidCause::Deadline,
                    1 => VoidCause::ChildFailed,
                    2 => VoidCause::Gate,
                    3 => VoidCause::Other,
                    _ => return Err(LedgerRefusal::Broken { line: k }),
                },
                class: match d.u8("class").map_err(broken)? {
                    0 => VoidClass::Stopped,
                    1 => VoidClass::Opening,
                    2 => VoidClass::Closing,
                    _ => return Err(LedgerRefusal::Broken { line: k }),
                },
            },
            8 => Line::Record,
            _ => return Err(LedgerRefusal::Broken { line: k }),
        };
        d.finish().map_err(broken)?;
        if line.encode(k, prev) != bytes {
            return Err(LedgerRefusal::Broken { line: k });
        }
        Ok(line)
    }
}

/// The link before a pair's line 0, binding the pair.
#[inline]
fn genesis(pair: Pair) -> Digest {
    let (tag, index) = pair.parts();
    Digest::of(LEDGER_DOMAIN, &[b"genesis", &[tag], &index.to_le_bytes()])
}

/// The link after a line's bytes.
#[inline]
fn link(bytes: &[u8]) -> Digest {
    Digest::of(LEDGER_DOMAIN, &[bytes])
}

/// Why the ledger refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerRefusal {
    /// The line is not the next the rules allow.
    OutOfOrder,
    /// Every look of the admission set is spent.
    HeldOutSpent,
    /// The report set is read.
    ReportSpent,
    /// The chain is not the version's: not the chain file's, or not [`M6_CHAIN_V1`].
    ChainMoved,
    /// No practice ledger under the root holds `report-done`.
    NoRehearsal,
    /// An earlier real pair is missing, or does not end in a void that opens a new pair.
    PairOrder,
    /// A real pair holds a `record` or a closing void.
    VersionClosed,
    /// Another writer made the line first.
    LedgerRace,
    /// A real pair's ledger without the real sets' capability.
    NoCapability,
    /// Line `line` does not decode or does not chain.
    Broken {
        /// The line.
        line: u64,
    },
    /// A recomputed list is not the one the ledger recorded (a defect).
    ReplayMismatch {
        /// The look.
        look: u8,
    },
    /// The chain given is not the one the ledger runs.
    NotTheChain,
    /// A plan a fold needs is not among its read's labels (a defect of the list).
    Unlabelled,
    /// The log's I/O failed.
    Log(String),
    /// A token was not minted.
    Mint(MintRefusal),
    /// The learner refused.
    Learn(LearnRefusal),
}

impl fmt::Display for LedgerRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerRefusal::OutOfOrder => write!(f, "ledger refused: OutOfOrder"),
            LedgerRefusal::HeldOutSpent => {
                write!(f, "ledger refused: HeldOutSpent, every look is spent")
            }
            LedgerRefusal::ReportSpent => {
                write!(f, "ledger refused: ReportSpent, the report set is read")
            }
            LedgerRefusal::ChainMoved => write!(
                f,
                "ledger refused: ChainMoved, the chain is not the version's"
            ),
            LedgerRefusal::NoRehearsal => write!(
                f,
                "ledger refused: NoRehearsal, no practice ledger under the root holds report-done"
            ),
            LedgerRefusal::PairOrder => write!(
                f,
                "ledger refused: PairOrder, an earlier pair is missing or did not end in an opening void"
            ),
            LedgerRefusal::VersionClosed => write!(
                f,
                "ledger refused: VersionClosed, a real pair holds a record or a closing void"
            ),
            LedgerRefusal::LedgerRace => write!(
                f,
                "ledger refused: LedgerRace, another writer made the line first"
            ),
            LedgerRefusal::NoCapability => write!(
                f,
                "ledger refused: a real pair's ledger without the real sets' capability"
            ),
            LedgerRefusal::Broken { line } => {
                write!(f, "ledger refused: line {line} does not decode or chain")
            }
            LedgerRefusal::ReplayMismatch { look } => write!(
                f,
                "ledger defect: look {look}'s recomputed list is not the one recorded"
            ),
            LedgerRefusal::NotTheChain => {
                write!(f, "ledger refused: the chain given is not the one it runs")
            }
            LedgerRefusal::Unlabelled => write!(
                f,
                "ledger defect: a plan a fold needs is not among its read's labels"
            ),
            LedgerRefusal::Log(e) => write!(f, "ledger refused: {e}"),
            LedgerRefusal::Mint(e) => write!(f, "ledger refused: {e}"),
            LedgerRefusal::Learn(e) => write!(f, "ledger refused: {e}"),
        }
    }
}

impl std::error::Error for LedgerRefusal {}

impl From<LearnRefusal> for LedgerRefusal {
    fn from(e: LearnRefusal) -> LedgerRefusal {
        LedgerRefusal::Learn(e)
    }
}

impl From<PoolRefusal> for LedgerRefusal {
    fn from(e: PoolRefusal) -> LedgerRefusal {
        LedgerRefusal::Learn(LearnRefusal::Pool(e))
    }
}

impl From<MintRefusal> for LedgerRefusal {
    fn from(e: MintRefusal) -> LedgerRefusal {
        LedgerRefusal::Mint(e)
    }
}

/// Why a log's I/O failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogFault {
    /// The line exists: another writer made it first.
    Exists,
    /// Any other failure.
    Io(String),
}

impl From<LogFault> for LedgerRefusal {
    fn from(e: LogFault) -> LedgerRefusal {
        match e {
            LogFault::Exists => LedgerRefusal::LedgerRace,
            LogFault::Io(e) => LedgerRefusal::Log(e),
        }
    }
}

/// One pair's line files under a version's root.
pub type PairLines = (Pair, Vec<Vec<u8>>);

/// The byte I/O of one pair's ledger and its version's root, which the lab implements over files
/// (design §13.4, rule 4).
pub trait DurableLog {
    /// This pair's line files, from line 0 up to the first missing one.
    /// # Errors
    /// An I/O failure.
    fn lines(&self) -> Result<Vec<Vec<u8>>, LogFault>;

    /// Creates this pair's line `k` holding `bytes`, durably, before it returns.
    /// # Errors
    /// [`LogFault::Exists`] when line `k` exists, or an I/O failure.
    fn append(&mut self, k: u64, bytes: &[u8]) -> Result<(), LogFault>;

    /// Every pair's line files under the version's root, this pair's among them.
    /// # Errors
    /// An I/O failure.
    fn root(&self) -> Result<Vec<PairLines>, LogFault>;

    /// The version's chain file: created holding `digest` when it does not exist, then read.
    /// # Errors
    /// An I/O failure.
    fn chain(&mut self, digest: &Digest) -> Result<Digest, LogFault>;
}

/// The lines `raw` hold, checked link by link from `pair`'s genesis, with the head after them.
#[inline]
fn read_lines(pair: Pair, raw: &[Vec<u8>]) -> Result<(Vec<Line>, Digest), LedgerRefusal> {
    let mut head = genesis(pair);
    let mut lines = Vec::with_capacity(raw.len());
    for (k, bytes) in raw.iter().enumerate() {
        let line = Line::decode(bytes, k as u64, &head)?;
        head = link(bytes);
        lines.push(line);
    }
    Ok((lines, head))
}

/// The head of `pair`'s ledger after the lines `raw` hold, each checked link by link from the
/// pair's genesis: what a store's revision records when no session holds the ledger (the lab's
/// `init`, `train`, `rollback` and `verify`, which read it without its lock).
/// # Errors
/// A line that does not decode or does not chain.
#[inline]
pub fn ledger_head(pair: Pair, raw: &[Vec<u8>]) -> Result<Digest, LedgerRefusal> {
    read_lines(pair, raw).map(|(_, head)| head)
}

/// The last session's `open`: a replaying child's open is part of the session it replays.
#[inline]
const fn last_open(lines: &[Line]) -> Option<usize> {
    let mut i = lines.len();
    while i > 0 {
        i -= 1;
        if matches!(lines[i], Line::Open { replay: false, .. }) {
            return Some(i);
        }
    }
    None
}

/// The candidate and comparator look `look` must name after `bits`, the verdicts of looks
/// `1..look`, under the chain `ids` (`m6-looks v1`, design §13.4, rule 3).
#[inline]
#[must_use]
pub fn look_pair(
    look: u8,
    bits: &[bool],
    ids: &[Digest; 1 + ROUNDS as usize],
) -> Option<(Digest, Digest)> {
    let v0 = ids[0];
    let mut comparator = v0;
    let pairs = [(1usize, ids[1]), (3usize, ids[2]), (4usize, ids[3])];
    let mut i = 0;
    while i < pairs.len() {
        let (n, member) = pairs[i];
        let bit_n = if n - 1 < bits.len() {
            bits[n - 1]
        } else {
            false
        };
        if (look as usize) > n && bit_n {
            comparator = member;
        }
        i += 1;
    }
    match look {
        1 => Some((ids[1], v0)),
        2 => Some((control_id(), v0)),
        3..=5 => Some((ids[(look as usize) - 1], comparator)),
        _ => None,
    }
}

/// Whether `line` may follow `lines` in one pair's ledger under the chain `ids` (design §13.4,
/// rules 3 to 6). The cross-pair checks are [`cross_pair`]'s.
/// # Errors
/// The rule's refusal.
#[inline]
pub fn next_line(
    lines: &[Line],
    line: &Line,
    ids: &[Digest; 1 + ROUNDS as usize],
) -> Result<(), LedgerRefusal> {
    let mut ended = false;
    let mut begun = 0usize;
    let mut decided = 0usize;
    let mut report_begun = false;
    let mut report_done = false;
    let mut retired = false;
    let mut replaying = false;
    for l in lines {
        match l {
            Line::Record
            | Line::Void {
                class: VoidClass::Opening | VoidClass::Closing,
                ..
            } => ended = true,
            Line::LookBegin { .. } => begun += 1,
            Line::LookVerdict { .. } => decided += 1,
            Line::ReportBegin { .. } => report_begun = true,
            Line::ReportDone { .. } => report_done = true,
            Line::Retire => retired = true,
            Line::Open { replay: true, .. } => replaying = true,
            _ => {}
        }
    }
    if ended {
        return Err(LedgerRefusal::OutOfOrder);
    }
    let last = lines.last();
    let stopped = matches!(
        last,
        Some(Line::Void {
            class: VoidClass::Stopped,
            ..
        })
    );
    let open = last_open(lines).filter(|_| !stopped);
    let session = open.map_or(&[][..], |at| &lines[at..]);
    let replayed = |look: u8| {
        let mut after_report = false;
        for l in session {
            if matches!(l, Line::ReportDone { .. }) {
                after_report = true;
            } else if after_report && *l == (Line::Replay { look }) {
                return true;
            }
        }
        false
    };
    let ok = |cond: bool| {
        if cond {
            Ok(())
        } else {
            Err(LedgerRefusal::OutOfOrder)
        }
    };
    match line {
        Line::Open { replay: false, .. } => ok(open.is_none()),
        Line::Open {
            replay: true,
            store,
            ..
        } => {
            let mut seen_store = false;
            for l in lines {
                if let Line::Open { store: s, .. } = l
                    && s == store
                {
                    seen_store = true;
                    break;
                }
            }
            ok(open.is_some() && retired && !replaying && !seen_store)
        }
        Line::LookBegin {
            look,
            candidate,
            comparator,
            ..
        } => {
            if open.is_none() || retired || report_begun || begun > decided {
                return Err(LedgerRefusal::OutOfOrder);
            }
            if begun >= usize::from(LOOKS) {
                return Err(LedgerRefusal::HeldOutSpent);
            }
            let mut bits = Vec::with_capacity(decided);
            for l in lines {
                if let Line::LookVerdict { bit } = l {
                    bits.push(*bit);
                }
            }
            ok(usize::from(*look) == begun + 1
                && look_pair(*look, &bits, ids) == Some((*candidate, *comparator)))
        }
        Line::LookVerdict { .. } => ok(matches!(last, Some(Line::LookBegin { .. }))),
        Line::ReportBegin { .. } => {
            if report_begun {
                return Err(LedgerRefusal::ReportSpent);
            }
            ok(open.is_some() && decided == usize::from(LOOKS) && begun == decided)
        }
        Line::ReportDone { .. } => ok(matches!(last, Some(Line::ReportBegin { .. }))),
        Line::Replay { look } => ok(open.is_some()
            && report_done
            && (!retired || replaying)
            && *look >= 1
            && usize::from(*look) <= decided),
        Line::Retire => {
            let mut all_replayed = true;
            for look in 1..=LOOKS {
                if !replayed(look) {
                    all_replayed = false;
                    break;
                }
            }
            ok(open.is_some() && report_done && !retired && all_replayed)
        }
        Line::Void { cause, class } => ok(open.is_some() && void_class(session, *cause) == *class),
        Line::Record => ok(open.is_some() && retired && replaying),
    }
}

/// The checks across real pairs (design §13.4, rule 4), which the ledger runs before it writes a
/// real pair `target`'s `open`: one pure function of every pair's recorded lines.
/// # Errors
/// [`LedgerRefusal::NoRehearsal`], [`LedgerRefusal::VersionClosed`] or
/// [`LedgerRefusal::PairOrder`].
#[inline]
pub fn cross_pair(target: u32, pairs: &[(Pair, Vec<Line>)]) -> Result<(), LedgerRefusal> {
    let lines_of = |pair: Pair| {
        for (p, l) in pairs {
            if *p == pair {
                return Some(l);
            }
        }
        None
    };
    let mut rehearsed = false;
    let mut closed = false;
    let mut later = false;
    for (p, lines) in pairs {
        match p {
            Pair::Practice(_) => {
                for l in lines {
                    if matches!(l, Line::ReportDone { .. }) {
                        rehearsed = true;
                        break;
                    }
                }
            }
            Pair::Real(i) => {
                if *i > target && !lines.is_empty() {
                    later = true;
                }
                for l in lines {
                    if matches!(
                        l,
                        Line::Record
                            | Line::Void {
                                class: VoidClass::Closing,
                                ..
                            }
                    ) {
                        closed = true;
                        break;
                    }
                }
            }
        }
    }
    if !rehearsed {
        return Err(LedgerRefusal::NoRehearsal);
    }
    if closed {
        return Err(LedgerRefusal::VersionClosed);
    }
    if !(1..=3).contains(&target) || later {
        return Err(LedgerRefusal::PairOrder);
    }
    if lines_of(Pair::Real(target)).is_none_or(Vec::is_empty) {
        for i in 1..target {
            let opened = matches!(
                lines_of(Pair::Real(i)).and_then(|l| l.last()),
                Some(Line::Void {
                    class: VoidClass::Opening,
                    ..
                })
            );
            if !opened {
                return Err(LedgerRefusal::PairOrder);
            }
        }
    }
    Ok(())
}

/// The proof that a look admitted a candidate (design §13.4), bound to its pair, look,
/// candidate and comparator: only a durable `look-verdict` of 1, or a replay that equals one,
/// yields it, never for look 2, and [`Store::activate`] consumes it.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelAdmitted {
    pair: Pair,
    look: u8,
    candidate: Digest,
    comparator: Digest,
}

impl ModelAdmitted {
    /// The pair.
    #[inline]
    #[must_use]
    pub const fn pair(&self) -> Pair {
        self.pair
    }

    /// The look.
    #[inline]
    #[must_use]
    pub const fn look(&self) -> u8 {
        self.look
    }

    /// The admitted candidate.
    #[inline]
    #[must_use]
    pub const fn candidate(&self) -> Digest {
        self.candidate
    }

    /// The comparator it beat.
    #[inline]
    #[must_use]
    pub const fn comparator(&self) -> Digest {
        self.comparator
    }
}

/// Why a candidate was not admitted (design §13.4). A look's cause is never written before the
/// report; it is recomputed by replay after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRefusal {
    /// No untied pool.
    NoUntied,
    /// The sign test refused: too few wins.
    Losses,
    /// The summed gains are lower than the comparator's.
    Gain,
}

impl fmt::Display for ModelRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelRefusal::NoUntied => write!(f, "not admitted: NoUntied, every pool tied"),
            ModelRefusal::Losses => write!(f, "not admitted: Losses, the sign test refused"),
            ModelRefusal::Gain => write!(
                f,
                "not admitted: Gain, its summed gains are below its comparator's"
            ),
        }
    }
}

impl std::error::Error for ModelRefusal {}

/// A ranker the ledger names by id: a chain member's state, or the control.
#[inline]
fn ranker_of<'c>(id: &Digest, chain: &'c [Version]) -> Option<Ranker<'c>> {
    if *id == control_id() {
        return Some(Ranker::Control);
    }
    for v in chain {
        if v.id == *id {
            return Some(Ranker::Model(&v.state));
        }
    }
    None
}

/// Each pool's top [`JUDGED`] under `x` and under `a`.
type Tops = Vec<(PoolId, [SchedulePlan; JUDGED], [SchedulePlan; JUDGED])>;

/// What a look's token and figures are bound to: the ledger's pair, the look, its candidate and
/// comparator, and the digest of the list its `look-begin` recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Named {
    pair: Pair,
    look: u8,
    candidate: Digest,
    comparator: Digest,
    list: Digest,
}

impl Named {
    /// The look's `look-begin` line.
    #[inline]
    const fn begin(&self) -> Line {
        Line::LookBegin {
            look: self.look,
            candidate: self.candidate,
            comparator: self.comparator,
            list: self.list,
        }
    }

    /// The admission a bit of `bit` yields: none for a 0, nor for look 2's control.
    #[inline]
    const fn admitted(&self, bit: bool) -> Option<ModelAdmitted> {
        if bit && self.look != 2 {
            Some(ModelAdmitted {
                pair: self.pair,
                look: self.look,
                candidate: self.candidate,
                comparator: self.comparator,
            })
        } else {
            None
        }
    }
}

/// A look begun: its line is durable and its token minted.
#[derive(Debug)]
pub struct Look {
    named: Named,
    tops: Tops,
    token: LookToken,
}

#[cfg(test)]
impl Look {
    /// Its token (tests).
    pub(super) const fn token(&self) -> &LookToken {
        &self.token
    }

    /// Each pool's two tops (tests).
    pub(super) fn tops(&self) -> &Tops {
        &self.tops
    }

    /// Its token, taken out (tests: a ledger-minted token's scope).
    pub(super) fn into_token(self) -> LookToken {
        self.token
    }
}

/// A look replayed: its `replay` line is durable and its token minted.
#[derive(Debug)]
pub struct Replayed {
    named: Named,
    tops: Tops,
    token: ReplayToken,
}

/// The report's read: its `report-begin` line is durable and its token minted.
#[derive(Debug)]
pub struct Read {
    pools: Vec<(PoolId, Vec<SchedulePlan>)>,
    token: ReadToken,
}

/// One look's figures: each pool's two outcomes, which no output shows before the report, bound
/// to the pair, look, ids and list of the token they were read under. Only a run look or replay
/// builds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookFigures {
    named: Named,
    pools: Vec<(PoolOutcome, PoolOutcome)>,
    comparison: Comparison,
    verdict: Result<(), ModelRefusal>,
}

impl LookFigures {
    /// The pair whose ledger minted its token.
    #[inline]
    #[must_use]
    pub const fn pair(&self) -> Pair {
        self.named.pair
    }

    /// The look.
    #[inline]
    #[must_use]
    pub const fn look(&self) -> u8 {
        self.named.look
    }

    /// The candidate's id.
    #[inline]
    #[must_use]
    pub const fn candidate(&self) -> Digest {
        self.named.candidate
    }

    /// The comparator's id.
    #[inline]
    #[must_use]
    pub const fn comparator(&self) -> Digest {
        self.named.comparator
    }

    /// The digest of the list its token read, as its `look-begin` recorded it.
    #[inline]
    #[must_use]
    pub const fn list(&self) -> Digest {
        self.named.list
    }

    /// Each pool's candidate outcome, then its comparator's.
    #[inline]
    #[must_use]
    pub fn pools(&self) -> &[(PoolOutcome, PoolOutcome)] {
        &self.pools
    }

    /// The comparison.
    #[inline]
    #[must_use]
    pub const fn comparison(&self) -> Comparison {
        self.comparison
    }

    /// Its verdict: `Ok` admitted, else the cause.
    #[inline]
    pub const fn verdict(&self) -> Result<(), ModelRefusal> {
        self.verdict
    }

    /// The bit.
    #[inline]
    #[must_use]
    pub const fn bit(&self) -> bool {
        self.verdict.is_ok()
    }

    /// Its figures file's bytes: the pair, look, candidate, comparator and list it is bound to,
    /// each pool's two keys and gains, then the verdict.
    #[inline]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        let (tag, index) = self.named.pair.parts();
        e.u8(tag);
        e.u32(index);
        e.u8(self.named.look);
        e.digest(&self.named.candidate);
        e.digest(&self.named.comparator);
        e.digest(&self.named.list);
        e.u32(len32(self.pools.len()));
        for (x, a) in &self.pools {
            for o in [x, a] {
                let (class, t, z) = o.key();
                e.u8(class);
                e.u64(t);
                e.u64(z);
                e.u64(o.gain().0);
                e.u64(o.gain().1);
            }
        }
        e.u8(match self.verdict {
            Ok(()) => 0,
            Err(ModelRefusal::NoUntied) => 1,
            Err(ModelRefusal::Losses) => 2,
            Err(ModelRefusal::Gain) => 3,
        });
        e.finish()
    }
}

#[cfg(test)]
impl LookFigures {
    /// These figures renamed as look `look` (tests: the store's count budgets).
    pub(super) const fn relooked(mut self, look: u8) -> LookFigures {
        self.named.look = look;
        self
    }
}

/// The ledger of one pair (design §13.4, rule 4): its lines, its head, the chain it runs and,
/// for a real pair, the real sets' capability its mints need. The runner's ledger writes only
/// `void` and `record`, and mints and admits nothing.
pub struct Ledger<L> {
    log: L,
    pair: Pair,
    lines: Vec<Line>,
    head: Digest,
    pin: ChainPin,
    real: Option<RealSets>,
    runner: bool,
}

impl<L> fmt::Debug for Ledger<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ledger")
            .field("pair", &self.pair)
            .field("lines", &self.lines)
            .field("head", &self.head)
            .finish_non_exhaustive()
    }
}

impl<L: DurableLog> Ledger<L> {
    /// Practice pair `j`'s ledger over `log`, running the chain `pin` names.
    /// # Errors
    /// A line that does not decode, chain or follow the rules.
    #[inline]
    pub fn practice(log: L, j: u32, pin: ChainPin) -> Result<Ledger<L>, LedgerRefusal> {
        Ledger::load(log, Pair::Practice(j), pin, None, false)
    }

    /// Real pair `i`'s ledger over `log`, holding the real sets' capability to lend its mints. It
    /// runs [`M6_CHAIN_V1`] alone.
    /// # Errors
    /// [`LedgerRefusal::ChainMoved`] while no chain is pinned, or a line's refusal.
    #[inline]
    pub fn real(log: L, i: u32, sets: RealSets) -> Result<Ledger<L>, LedgerRefusal> {
        let pin = M6_CHAIN_V1.ok_or(LedgerRefusal::ChainMoved)?;
        Ledger::load(log, Pair::Real(i), pin, Some(sets), false)
    }

    /// `pair`'s ledger for the runner, which writes only `void` and `record` and mints and admits
    /// nothing: any other line, mint or admission is refused [`LedgerRefusal::NoCapability`].
    /// # Errors
    /// A line's refusal.
    #[inline]
    pub fn runner(log: L, pair: Pair, pin: ChainPin) -> Result<Ledger<L>, LedgerRefusal> {
        Ledger::load(log, pair, pin, None, true)
    }

    #[inline]
    fn load(
        log: L,
        pair: Pair,
        pin: ChainPin,
        real: Option<RealSets>,
        runner: bool,
    ) -> Result<Ledger<L>, LedgerRefusal> {
        let raw = log.lines()?;
        let (lines, head) = read_lines(pair, &raw)?;
        for k in 0..lines.len() {
            next_line(&lines[..k], &lines[k], &pin.ids)
                .map_err(|_| LedgerRefusal::Broken { line: k as u64 })?;
        }
        Ok(Ledger {
            log,
            pair,
            lines,
            head,
            pin,
            real,
            runner,
        })
    }

    /// Its pair.
    #[inline]
    #[must_use]
    pub const fn pair(&self) -> Pair {
        self.pair
    }

    /// Its lines.
    #[inline]
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// The digest after its last line.
    #[inline]
    #[must_use]
    pub const fn head(&self) -> Digest {
        self.head
    }

    /// The bits of its looks, in order.
    #[inline]
    #[must_use]
    pub fn bits(&self) -> Vec<bool> {
        let mut out = Vec::new();
        for l in &self.lines {
            if let Line::LookVerdict { bit } = l {
                out.push(*bit);
            }
        }
        out
    }

    /// Refuses a mint or an admission on the runner's ledger, or on a real pair's without the
    /// real sets' capability.
    #[inline]
    const fn minting(&self) -> Result<(), LedgerRefusal> {
        if self.runner || (matches!(self.pair, Pair::Real(_)) && self.real.is_none()) {
            return Err(LedgerRefusal::NoCapability);
        }
        Ok(())
    }

    /// Appends `line` if the rules allow it, durably; the runner's ledger takes only `void` and
    /// `record`.
    #[inline]
    fn append(&mut self, line: Line) -> Result<(), LedgerRefusal> {
        if self.runner && !matches!(line, Line::Void { .. } | Line::Record) {
            return Err(LedgerRefusal::NoCapability);
        }
        next_line(&self.lines, &line, &self.pin.ids)?;
        let k = self.lines.len() as u64;
        let bytes = line.encode(k, &self.head);
        self.log.append(k, &bytes)?;
        self.head = link(&bytes);
        self.lines.push(line);
        Ok(())
    }

    #[inline]
    fn chain_is_ours(&self, chain: &[Version]) -> Result<(), LedgerRefusal> {
        if ChainPin::of(chain) == Some(self.pin) {
            Ok(())
        } else {
            Err(LedgerRefusal::NotTheChain)
        }
    }

    /// Opens a session on the store `store`, after the checks across real pairs and the chain
    /// file's (design §13.4, rules 2 and 4).
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`], [`LedgerRefusal::NoCapability`], [`cross_pair`]'s,
    /// [`LedgerRefusal::ChainMoved`] or the log's.
    #[inline]
    pub fn open(
        &mut self,
        store: Digest,
        build: &str,
        tests_log: Digest,
        replay: bool,
    ) -> Result<(), LedgerRefusal> {
        let line = Line::Open {
            store,
            build: build.to_owned(),
            chain: self.pin.digest(),
            tests_log,
            replay,
        };
        self.minting()?;
        next_line(&self.lines, &line, &self.pin.ids)?;
        if let Pair::Real(i) = self.pair {
            let mut pairs = Vec::new();
            for (pair, raw) in self.log.root()? {
                pairs.push((pair, read_lines(pair, &raw)?.0));
            }
            cross_pair(i, &pairs)?;
            if Some(self.pin) != M6_CHAIN_V1 {
                return Err(LedgerRefusal::ChainMoved);
            }
        }
        if self.log.chain(&self.pin.digest())? != self.pin.digest() {
            return Err(LedgerRefusal::ChainMoved);
        }
        self.append(line)
    }

    #[inline]
    fn tops(
        &self,
        set: SetId,
        x: &Digest,
        a: &Digest,
        chain: &[Version],
        sets: &Sets,
    ) -> Result<(Tops, Vec<[u8; 12]>), LedgerRefusal> {
        let (Some(rx), Some(ra)) = (ranker_of(x, chain), ranker_of(a, chain)) else {
            return Err(LedgerRefusal::NotTheChain);
        };
        let raw_pools = sets.pools(set)?;
        let mut tops = Vec::with_capacity(raw_pools.len());
        let mut list = BTreeSet::new();
        for (p, plans) in raw_pools.iter().enumerate() {
            let pool = PoolId {
                set,
                pool: len32(p),
            };
            let (tx, ta) = (rx.top_k(&pool, plans)?, ra.top_k(&pool, plans)?);
            for plan in &tx {
                list.insert(plan.encode());
            }
            for plan in &ta {
                list.insert(plan.encode());
            }
            tops.push((pool, tx, ta));
        }
        Ok((tops, list.into_iter().collect()))
    }

    /// Begins look `look` of `candidate` against `comparator`: the ledger makes the list (each
    /// pool's top [`JUDGED`] under both, sorted and deduplicated), writes `look-begin`, and only
    /// then mints the look's token.
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`], [`LedgerRefusal::HeldOutSpent`], or the mint's.
    #[inline]
    pub fn begin_look(
        &mut self,
        look: u8,
        candidate: Digest,
        comparator: Digest,
        chain: &[Version],
        sets: &Sets,
    ) -> Result<Look, LedgerRefusal> {
        self.chain_is_ours(chain)?;
        let probe = Line::LookBegin {
            look,
            candidate,
            comparator,
            list: Digest([0; 32]),
        };
        next_line(&self.lines, &probe, &self.pin.ids)?;
        let set = self.pair.looks();
        let (tops, list) = self.tops(set, &candidate, &comparator, chain, sets)?;
        self.can_mint(list.len(), LOOK_LIST_CAP)?;
        let named = Named {
            pair: self.pair,
            look,
            candidate,
            comparator,
            list: list_digest(&list),
        };
        self.append(named.begin())?;
        let token = match (self.pair, &self.real) {
            (Pair::Practice(j), _) => LookToken::mint_practice(PracticeSet::HeldOut(j), &list)?,
            (Pair::Real(i), Some(real)) => LookToken::mint_real(real, i, &list)?,
            (Pair::Real(_), None) => return Err(LedgerRefusal::NoCapability),
        };
        Ok(Look { named, tops, token })
    }
}

impl<L: DurableLog> Ledger<L> {
    /// Refuses before a line is written what its mint would refuse after it.
    #[inline]
    fn can_mint(&self, len: usize, cap: usize) -> Result<(), LedgerRefusal> {
        self.minting()?;
        if len > cap {
            return Err(LedgerRefusal::Mint(MintRefusal::TooLong { len, cap }));
        }
        Ok(())
    }

    /// Writes the bit of the look `look` ran, then yields its admission when it is 1 and the look
    /// is not the control's.
    /// # Errors
    /// [`LedgerRefusal::NoCapability`] as for a mint, and [`LedgerRefusal::OutOfOrder`] unless
    /// the figures are this pair's and the last line is their look's `look-begin`, naming their
    /// candidate, comparator and list.
    #[inline]
    pub fn verdict(&mut self, look: &LookFigures) -> Result<Option<ModelAdmitted>, LedgerRefusal> {
        self.minting()?;
        let named = look.named;
        if named.pair != self.pair || self.lines.last() != Some(&named.begin()) {
            return Err(LedgerRefusal::OutOfOrder);
        }
        self.append(Line::LookVerdict { bit: look.bit() })?;
        Ok(named.admitted(look.bit()))
    }

    /// Begins the report: the list is every plan of the report set's pools, written in
    /// `report-begin` before its token is minted.
    /// # Errors
    /// [`LedgerRefusal::ReportSpent`], [`LedgerRefusal::OutOfOrder`] before the last look's
    /// verdict, or a pool's or the mint's.
    #[inline]
    pub fn begin_report(&mut self, sets: &Sets) -> Result<Read, LedgerRefusal> {
        next_line(
            &self.lines,
            &Line::ReportBegin {
                list: Digest([0; 32]),
            },
            &self.pin.ids,
        )?;
        let set = self.pair.report();
        let raw_pools = sets.pools(set)?;
        let mut pools = Vec::with_capacity(raw_pools.len());
        let mut list_set = BTreeSet::new();
        for (p, plans) in raw_pools.into_iter().enumerate() {
            for plan in &plans {
                list_set.insert(plan.encode());
            }
            pools.push((
                PoolId {
                    set,
                    pool: len32(p),
                },
                plans,
            ));
        }
        let list: Vec<[u8; 12]> = list_set.into_iter().collect();
        self.can_mint(list.len(), READ_LIST_CAP)?;
        self.append(Line::ReportBegin {
            list: list_digest(&list),
        })?;
        let token = match (self.pair, &self.real) {
            (Pair::Practice(j), _) => ReadToken::mint_practice(PracticeSet::Report(j), &list)?,
            (Pair::Real(i), Some(real)) => ReadToken::mint_real(real, i, &list)?,
            (Pair::Real(_), None) => return Err(LedgerRefusal::NoCapability),
        };
        Ok(Read { pools, token })
    }

    /// Writes `report-done` with the report file's digest.
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`] unless the last line is `report-begin`.
    #[inline]
    pub fn report_done(&mut self, report: Digest) -> Result<(), LedgerRefusal> {
        self.append(Line::ReportDone { report })
    }

    /// Replays look `look` after the report: its list recomputed from the recorded candidate
    /// and comparator must have the recorded digest, then `replay` is written and its token
    /// minted. It spends nothing.
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`] before `report-done` or for an unrecorded look,
    /// [`LedgerRefusal::ReplayMismatch`] (a defect), or the mint's.
    #[inline]
    pub fn begin_replay(
        &mut self,
        look: u8,
        chain: &[Version],
        sets: &Sets,
    ) -> Result<Replayed, LedgerRefusal> {
        self.chain_is_ours(chain)?;
        next_line(&self.lines, &Line::Replay { look }, &self.pin.ids)?;
        let mut begins = Vec::new();
        let mut decided = 0usize;
        for l in &self.lines {
            match l {
                Line::LookBegin {
                    candidate,
                    comparator,
                    list,
                    ..
                } => begins.push((*candidate, *comparator, *list)),
                Line::LookVerdict { .. } => decided += 1,
                _ => {}
            }
        }
        let (Some(&(candidate, comparator, digest)), true) = (
            begins.get(usize::from(look).wrapping_sub(1)),
            (1..=decided).contains(&usize::from(look)),
        ) else {
            return Err(LedgerRefusal::OutOfOrder);
        };
        let set = self.pair.looks();
        let (tops, list) = self.tops(set, &candidate, &comparator, chain, sets)?;
        if list_digest(&list) != digest {
            return Err(LedgerRefusal::ReplayMismatch { look });
        }
        self.can_mint(list.len(), LOOK_LIST_CAP)?;
        self.append(Line::Replay { look })?;
        let token = match (self.pair, &self.real) {
            (Pair::Practice(j), _) => ReplayToken::mint_practice(PracticeSet::HeldOut(j), &list)?,
            (Pair::Real(i), Some(real)) => ReplayToken::mint_real(real, i, &list)?,
            (Pair::Real(_), None) => return Err(LedgerRefusal::NoCapability),
        };
        Ok(Replayed {
            named: Named {
                pair: self.pair,
                look,
                candidate,
                comparator,
                list: digest,
            },
            tops,
            token,
        })
    }

    /// The admission a replay yields, with its figures. The replay must be this pair's and named
    /// by the last line, a `replay`, and its figures must name the `look-begin` it replays; its
    /// bit must equal that look's recorded `look-verdict`, and the admission is yielded when that
    /// is 1 and the look is not the control's. The replay is consumed, so one `replay` line
    /// yields at most one admission.
    /// # Errors
    /// [`LedgerRefusal::NoCapability`] as for a mint, [`LedgerRefusal::OutOfOrder`] for any other
    /// replay, and [`LedgerRefusal::ReplayMismatch`] when the replayed bit is not the recorded one
    /// (a defect).
    #[inline]
    pub fn replayed(
        &self,
        replay: ReplayFigures,
    ) -> Result<(Option<ModelAdmitted>, LookFigures), LedgerRefusal> {
        self.minting()?;
        let ReplayFigures { figures } = replay;
        let named = figures.named;
        let Some(at) = usize::from(named.look).checked_sub(1) else {
            return Err(LedgerRefusal::OutOfOrder);
        };
        let mut begin_idx = 0usize;
        let mut verdict_idx = 0usize;
        let mut begun = None;
        let mut recorded = None;
        for l in &self.lines {
            match l {
                Line::LookBegin { .. } => {
                    if begin_idx == at {
                        begun = Some(l);
                    }
                    begin_idx += 1;
                }
                Line::LookVerdict { bit } => {
                    if verdict_idx == at {
                        recorded = Some(*bit);
                    }
                    verdict_idx += 1;
                }
                _ => {}
            }
        }
        let (true, true, Some(recorded)) = (
            named.pair == self.pair && begun == Some(&named.begin()),
            self.lines.last() == Some(&Line::Replay { look: named.look }),
            recorded,
        ) else {
            return Err(LedgerRefusal::OutOfOrder);
        };
        if figures.bit() != recorded {
            return Err(LedgerRefusal::ReplayMismatch { look: named.look });
        }
        Ok((named.admitted(recorded), figures))
    }

    /// Retires the admission set.
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`] before `report-done` or after a retire.
    #[inline]
    pub fn retire(&mut self) -> Result<(), LedgerRefusal> {
        self.append(Line::Retire)
    }

    /// The runner's void on `cause`, classed by when it arose; it returns the class.
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`] with no session open.
    #[inline]
    pub fn void(&mut self, cause: VoidCause) -> Result<VoidClass, LedgerRefusal> {
        let at = last_open(&self.lines).ok_or(LedgerRefusal::OutOfOrder)?;
        let class = void_class(&self.lines[at..], cause);
        self.append(Line::Void { cause, class })?;
        Ok(class)
    }

    /// The runner's record of a finished session.
    /// # Errors
    /// [`LedgerRefusal::OutOfOrder`] before `retire`.
    #[inline]
    pub fn record(&mut self) -> Result<(), LedgerRefusal> {
        self.append(Line::Record)
    }
}

/// A replayed look's figures, which [`Ledger::replayed`] consumes. Only a run replay builds them,
/// and they are not `Clone`, so one replay is presented once.
#[derive(Debug, PartialEq, Eq)]
pub struct ReplayFigures {
    figures: LookFigures,
}

impl ReplayFigures {
    /// The replay's figures.
    #[inline]
    #[must_use]
    pub const fn figures(&self) -> &LookFigures {
        &self.figures
    }
}

/// The labels of `top`, from `by_id`, in its order; `None` when one is missing.
#[inline]
fn pick<'l>(
    by_id: &BTreeMap<Digest, &'l TrainLabel>,
    top: &[SchedulePlan; JUDGED],
) -> Option<[&'l TrainLabel; JUDGED]> {
    let mut picked = Vec::with_capacity(JUDGED);
    for p in top {
        picked.push(*by_id.get(&p.digest())?);
    }
    picked.try_into().ok()
}

/// A test's verdict for a look: admitted or not.
#[cfg(test)]
type Force = fn(u8) -> bool;

#[cfg(test)]
thread_local! {
    /// A test's forced verdicts, by look ([`forcing`]).
    static FORCED: Cell<Option<Force>> = const { Cell::new(None) };
}

/// Runs `body` with every look's and replay's verdict forced to `force(look)`, on this thread:
/// a forced admission is `Ok`, a forced refusal [`ModelRefusal::Losses`] (tests).
#[cfg(test)]
pub(super) fn forcing<R>(force: Force, body: impl FnOnce() -> R) -> R {
    let before = FORCED.with(|f| f.replace(Some(force)));
    let out = body();
    FORCED.with(|f| f.set(before));
    out
}

/// Folds the pools of a look's tops under `labels`.
#[inline]
fn fold_tops(
    labeller: &Labeller<'_>,
    named: Named,
    tops: &Tops,
    labels: &[TrainLabel],
) -> Result<LookFigures, LedgerRefusal> {
    let mut by_id = BTreeMap::new();
    for l in labels {
        by_id.insert(l.id(), l);
    }
    let mut pools = Vec::with_capacity(tops.len());
    let mut xs = Vec::with_capacity(tops.len());
    let mut as_ = Vec::with_capacity(tops.len());
    for (_, tx, ta) in tops {
        let (Some(lx), Some(la)) = (pick(&by_id, tx), pick(&by_id, ta)) else {
            return Err(LedgerRefusal::Unlabelled);
        };
        let ox = labeller.ev.pool_outcome(lx);
        let oa = labeller.ev.pool_outcome(la);
        xs.push(ox);
        as_.push(oa);
        pools.push((ox, oa));
    }
    let comparison = Comparison::of(&xs, &as_);
    let verdict = comparison.admits();
    #[cfg(test)]
    let verdict = match FORCED.with(Cell::get) {
        Some(force) if force(named.look) => Ok(()),
        Some(_) => Err(ModelRefusal::Losses),
        None => verdict,
    };
    Ok(LookFigures {
        named,
        verdict,
        pools,
        comparison,
    })
}

/// Runs a begun look: labels its token's list, once, then folds each pool's two tops and
/// compares them by the admission rule (design §13.4).
/// # Errors
/// Labelling's refusal.
#[inline]
pub fn run_look(labeller: &Labeller<'_>, look: Look) -> Result<LookFigures, LedgerRefusal> {
    let Look { named, tops, token } = look;
    let list = token.list().to_vec();
    let labels = labeller
        .label(LabelScope::HeldOut(token), &list)
        .map_err(|e| LedgerRefusal::Learn(LearnRefusal::Label(e)))?;
    fold_tops(labeller, named, &tops, &labels)
}

/// Runs a replayed look the same way, under its replay token.
/// # Errors
/// Labelling's refusal.
#[inline]
pub fn run_replay(
    labeller: &Labeller<'_>,
    replay: Replayed,
) -> Result<ReplayFigures, LedgerRefusal> {
    let Replayed { named, tops, token } = replay;
    let list = token.list().to_vec();
    let labels = labeller
        .label(LabelScope::Replay(token), &list)
        .map_err(|e| LedgerRefusal::Learn(LearnRefusal::Label(e)))?;
    Ok(ReplayFigures {
        figures: fold_tops(labeller, named, &tops, &labels)?,
    })
}

/// An arm of the report (design §13.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arm {
    /// v0: a uniform K-subset of each pool.
    U,
    /// `NextStep` or `Cadence` first.
    W1,
    /// `SettleAtStep ∧ NextStep` first, then W1's order.
    W2,
    /// Chain member v1, admitted or not.
    C,
    /// The active version at report time.
    Mf,
    /// The control.
    Ctl,
    /// All of each pool judged: the ceiling, reported and never tested.
    O,
}

impl Arm {
    /// Every arm, in report order.
    pub const ALL: [Arm; 7] = [Arm::U, Arm::W1, Arm::W2, Arm::C, Arm::Mf, Arm::Ctl, Arm::O];

    /// Its name in the report.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Arm::U => "U",
            Arm::W1 => "W1",
            Arm::W2 => "W2",
            Arm::C => "C",
            Arm::Mf => "Mf",
            Arm::Ctl => "Ctl",
            Arm::O => "O",
        }
    }
}

/// The report on the report set: every arm's outcome in every pool, and the criteria C1 to C3,
/// each a [`Comparison::beats`] at level [`REPORT_LEVEL`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pair: Pair,
    active: Digest,
    arms: Vec<(Arm, Vec<PoolOutcome>)>,
    /// The ids of the labels each ranked arm folded, pool by pool (tests).
    #[cfg(test)]
    pub(super) folded: Vec<(Arm, Vec<[Digest; JUDGED]>)>,
}

impl Report {
    /// The arm's outcomes, pool by pool.
    #[inline]
    #[must_use]
    pub fn arm(&self, arm: Arm) -> &[PoolOutcome] {
        for (a, o) in &self.arms {
            if *a == arm {
                return o;
            }
        }
        &[]
    }

    /// Mf against `against`, by the admission rule's test at [`REPORT_LEVEL`].
    #[inline]
    pub fn criterion(&self, against: Arm) -> (Comparison, Result<(), ModelRefusal>) {
        let c = Comparison::of(self.arm(Arm::Mf), self.arm(against));
        (c, c.shows())
    }

    /// The report file's text.
    #[inline]
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "aterm-aos-m6-report-v1");
        match self.pair {
            Pair::Real(i) => {
                let _ = writeln!(out, "pair pair-{i}");
            }
            Pair::Practice(j) => {
                let _ = writeln!(out, "pair practice-{j}");
            }
        }
        let _ = writeln!(out, "active {}", self.active.hex());
        let oracle = sums(self.arm(Arm::O));
        for &(arm, ref pools) in &self.arms {
            let (selections, gt, gz) = sums(pools);
            let _ = writeln!(
                out,
                "arm {} class0 {selections} gainT {gt} gainZ {gz} regretT {} regretZ {}",
                arm.name(),
                oracle.1.saturating_sub(gt),
                oracle.2.saturating_sub(gz),
            );
        }
        for (name, against) in [("C1", Arm::U), ("C2", Arm::W2), ("C3", Arm::W1)] {
            let (c, verdict) = self.criterion(against);
            let shown = fmt::from_fn(move |f| match verdict {
                Ok(()) => f.write_str("shown"),
                Err(e) => write!(f, "not-shown {e:?}"),
            });
            let _ = writeln!(
                out,
                "criterion {name} Mf-vs-{} wins {} losses {} gain {}/{} against {}/{} {shown}",
                against.name(),
                c.wins,
                c.losses,
                c.gain.0,
                c.gain.1,
                c.against.0,
                c.against.1,
            );
        }
        for p in 0..self.arm(Arm::O).len() {
            let _ = write!(out, "pool {p}");
            for &(arm, ref pools) in &self.arms {
                if let Some(o) = pools.get(p) {
                    let (class, t, z) = o.key();
                    let _ = write!(out, " {}:{class}/{t}/{z}", arm.name());
                }
            }
            out.push('\n');
        }
        out
    }
}

/// The count of selections and the summed gains of `pools`.
#[inline]
const fn sums(pools: &[PoolOutcome]) -> (usize, u64, u64) {
    let (mut n, mut t, mut z) = (0usize, 0u64, 0u64);
    let mut i = 0;
    while i < pools.len() {
        let o = pools[i];
        if o.class() == 0 {
            n += 1;
        }
        let g = o.gain();
        t += g.0;
        z += g.1;
        i += 1;
    }
    (n, t, z)
}

/// Runs the report's one read: labels every plan of the report set once, then folds every arm in
/// every pool (design §13.9).
/// # Errors
/// Labelling's refusal, or [`LedgerRefusal::NotTheChain`] for an active id the chain lacks.
#[inline]
pub fn run_report(
    labeller: &Labeller<'_>,
    read: Read,
    pair: Pair,
    chain: &[Version],
    active: &Digest,
) -> Result<Report, LedgerRefusal> {
    let Read { pools, token } = read;
    let list = token.list().to_vec();
    let labels = labeller
        .label(LabelScope::Report(token), &list)
        .map_err(|e| LedgerRefusal::Learn(LearnRefusal::Label(e)))?;
    let mut by_id = BTreeMap::new();
    for l in &labels {
        by_id.insert(l.id(), l);
    }
    let mut mf_opt = None;
    for v in chain {
        if v.id == *active {
            mf_opt = Some(v);
            break;
        }
    }
    let (Some(v0), Some(v1), Some(mf)) = (chain.first(), chain.get(1), mf_opt) else {
        return Err(LedgerRefusal::NotTheChain);
    };
    let ranked = [
        (Arm::U, Ranker::Model(&v0.state)),
        (Arm::W1, Ranker::W1),
        (Arm::W2, Ranker::W2),
        (Arm::C, Ranker::Model(&v1.state)),
        (Arm::Mf, Ranker::Model(&mf.state)),
        (Arm::Ctl, Ranker::Control),
    ];
    let mut arms: Vec<(Arm, Vec<PoolOutcome>)> = Vec::with_capacity(ranked.len() + 1);
    for &(a, _) in &ranked {
        arms.push((a, Vec::with_capacity(pools.len())));
    }
    arms.push((Arm::O, Vec::with_capacity(pools.len())));
    #[cfg(test)]
    let mut folded: Vec<(Arm, Vec<[Digest; JUDGED]>)> = {
        let mut f = Vec::with_capacity(ranked.len());
        for &(a, _) in &ranked {
            f.push((a, Vec::with_capacity(pools.len())));
        }
        f
    };
    let mut all = Vec::new();
    for (pool, plans) in &pools {
        for (i, (_, ranker)) in ranked.iter().enumerate() {
            let top = ranker.top_k(pool, plans)?;
            let picked = pick(&by_id, &top).ok_or(LedgerRefusal::Unlabelled)?;
            #[cfg(test)]
            folded[i].1.push(picked.map(TrainLabel::id));
            arms[i].1.push(labeller.ev.pool_outcome(picked));
        }
        all.clear();
        all.reserve(plans.len());
        for p in plans {
            let Some(&lbl) = by_id.get(&p.digest()) else {
                return Err(LedgerRefusal::Unlabelled);
            };
            all.push(lbl);
        }
        arms[ranked.len()].1.push(labeller.ev.oracle_outcome(&all));
    }
    Ok(Report {
        pair,
        active: *active,
        arms,
        #[cfg(test)]
        folded,
    })
}

/// The domain of a store's id, the digest of its `store.txt`.
pub const STORE_DOMAIN: &[u8] = b"aterm-aos-m6-store-v1\0";

/// The domain of an eval record's digest.
pub const EVAL_DOMAIN: &[u8] = b"aterm-aos-m6-eval-v1\0";

/// The domain of a report file's digest, which `report-done` records.
pub const REPORT_DOMAIN: &[u8] = b"aterm-aos-m6-report-v1\0";

/// The first line of `store.txt`.
const STORE_HEADER: &str = "aterm-aos-m6-store-v1";

/// The write steps a store's transitions take, in order, which the fault seam stops after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// `store.txt`.
    StoreFile,
    /// A version's directory, created whole.
    VersionFiles,
    /// A revision's file.
    Revision,
    /// The `active` pointer's rename.
    Pointer,
    /// An eval record.
    Eval,
    /// A look's figures file.
    Figures,
    /// The report.
    Report,
}

/// Why a store's I/O failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreFault {
    /// The file exists.
    Exists(String),
    /// The fault seam stopped after this step (tests).
    Stopped(Step),
    /// Any other failure.
    Io(String),
}

/// The byte I/O of one store, which the lab implements over files (design §13.5): every file is
/// written to a temp name, fsynced and renamed, and only the pointer is ever replaced.
pub trait DurableStore {
    /// `name`'s bytes, or `None` when it does not exist.
    /// # Errors
    /// An I/O failure.
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreFault>;

    /// The names directly under `dir`, sorted; none when it does not exist.
    /// # Errors
    /// An I/O failure.
    fn list(&self, dir: &str) -> Result<Vec<String>, StoreFault>;

    /// Creates `name` holding `bytes`, durably.
    /// # Errors
    /// [`StoreFault::Exists`], the seam's stop after `step`, or an I/O failure.
    fn create(&mut self, step: Step, name: &str, bytes: &[u8]) -> Result<(), StoreFault>;

    /// Creates the directory `dir` holding `files`, whole or not at all.
    /// # Errors
    /// [`StoreFault::Exists`], the seam's stop after `step`, or an I/O failure.
    fn create_dir(
        &mut self,
        step: Step,
        dir: &str,
        files: &[(&str, &[u8])],
    ) -> Result<(), StoreFault>;

    /// Replaces `name` by one rename, durably.
    /// # Errors
    /// The seam's stop after `step`, or an I/O failure.
    fn replace(&mut self, step: Step, name: &str, bytes: &[u8]) -> Result<(), StoreFault>;
}

/// Why a store refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRefusal {
    /// The last revision is not the pointer.
    StoreTorn,
    /// `store.txt` names another build.
    ForeignBuild {
        /// The build it names.
        found: String,
    },
    /// `store.txt` names another M6 or M4 policy.
    PolicyMismatch,
    /// A file does not hash to its name, does not decode, or is missing.
    StoreCorrupt {
        /// The file.
        file: String,
    },
    /// A write would pass a store budget.
    Budget {
        /// What would pass it.
        what: &'static str,
    },
    /// The token's comparator is not the active version.
    NotComparator,
    /// The store already holds an `admit` revision for the token's look.
    Republished {
        /// The look.
        look: u8,
    },
    /// The token names the control, which is never published.
    Control,
    /// The admission, eval record or figures, or the session's ledger, are another pair's.
    ForeignPair,
    /// The version is not retained here.
    NotRetained,
    /// The rollback target is neither v0 nor a member a look admitted.
    NotAdmitted,
    /// The learner refused.
    Learn(LearnRefusal),
    /// The store's I/O failed.
    Io(StoreFault),
}

impl fmt::Display for StoreRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreRefusal::StoreTorn => {
                write!(
                    f,
                    "store refused: StoreTorn, the last revision is not the pointer"
                )
            }
            StoreRefusal::ForeignBuild { found } => {
                write!(f, "store refused: ForeignBuild, it binds build {found}")
            }
            StoreRefusal::PolicyMismatch => write!(
                f,
                "store refused: PolicyMismatch, it binds another M6 or M4 policy"
            ),
            StoreRefusal::StoreCorrupt { file } => write!(
                f,
                "store refused: StoreCorrupt, {file} is missing, undecodable or not its name"
            ),
            StoreRefusal::Budget { what } => write!(f, "store refused: {what} past its budget"),
            StoreRefusal::NotComparator => write!(
                f,
                "publication refused: the admission's comparator is not the active version"
            ),
            StoreRefusal::Republished { look } => write!(
                f,
                "publication refused: look {look}'s admission is already published here"
            ),
            StoreRefusal::Control => {
                write!(f, "publication refused: the control is never published")
            }
            StoreRefusal::ForeignPair => {
                write!(f, "store refused: ForeignPair, it binds another pair")
            }
            StoreRefusal::NotRetained => write!(f, "store refused: the version is not retained"),
            StoreRefusal::NotAdmitted => write!(
                f,
                "rollback refused: the target is neither v0 nor a member a look admitted"
            ),
            StoreRefusal::Learn(e) => write!(f, "store refused: {e}"),
            StoreRefusal::Io(e) => write!(f, "store refused: {e:?}"),
        }
    }
}

impl std::error::Error for StoreRefusal {}

impl From<StoreFault> for StoreRefusal {
    fn from(e: StoreFault) -> StoreRefusal {
        StoreRefusal::Io(e)
    }
}

impl From<LearnRefusal> for StoreRefusal {
    fn from(e: LearnRefusal) -> StoreRefusal {
        StoreRefusal::Learn(e)
    }
}

/// Why a revision was appended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The store's first revision: v0 active and at the head.
    Init,
    /// A look's admission published.
    Admit,
    /// A train moved the head.
    Train,
    /// A rollback moved the pointer and the head.
    Rollback,
}

/// One revision of a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    /// Its number, from 0.
    pub n: u32,
    /// The active id after it.
    pub active: Digest,
    /// The chain head after it.
    pub head: Digest,
    /// Why.
    pub reason: Reason,
    /// For an admission, its look.
    pub look: Option<u8>,
    /// The ledger's head digest when it was written.
    pub ledger: Digest,
}

impl Revision {
    #[inline]
    fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.u32(self.n);
        e.digest(&self.active);
        e.digest(&self.head);
        e.opt(self.n.checked_sub(1), |e, p| e.u32(p));
        e.u8(match self.reason {
            Reason::Init => 0,
            Reason::Admit => 1,
            Reason::Train => 2,
            Reason::Rollback => 3,
        });
        e.opt(self.look, |e, l| e.u8(l));
        e.digest(&self.ledger);
        e.finish()
    }

    #[inline]
    fn decode(bytes: &[u8]) -> Option<Revision> {
        let mut d = Dec::new(bytes);
        let n = d.u32("n").ok()?;
        let active = d.digest("active").ok()?;
        let head = d.digest("head").ok()?;
        let parent = d.opt("parent", |d| d.u32("parent")).ok()?;
        let reason = match d.u8("reason").ok()? {
            0 => Reason::Init,
            1 => Reason::Admit,
            2 => Reason::Train,
            3 => Reason::Rollback,
            _ => return None,
        };
        let look = d.opt("look", |d| d.u8("look")).ok()?;
        let ledger = d.digest("ledger").ok()?;
        d.finish().ok()?;
        let r = Revision {
            n,
            active,
            head,
            reason,
            look,
            ledger,
        };
        (parent == n.checked_sub(1) && r.encode() == bytes).then_some(r)
    }
}

/// The pointer: the revision it is at, the active id and the chain head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// The revision.
    pub revision: u32,
    /// The active version.
    pub active: Digest,
    /// The chain head.
    pub head: Digest,
}

impl Pointer {
    #[inline]
    fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.u32(self.revision);
        e.digest(&self.active);
        e.digest(&self.head);
        e.finish()
    }

    #[inline]
    fn decode(bytes: &[u8]) -> Option<Pointer> {
        let mut d = Dec::new(bytes);
        let p = Pointer {
            revision: d.u32("revision").ok()?,
            active: d.digest("active").ok()?,
            head: d.digest("head").ok()?,
        };
        d.finish().ok()?;
        Some(p)
    }
}

/// One look's eval record (design §13.5): written once after its verdict, or after its replay
/// in a replaying child's store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvalRecord {
    /// The candidate's id.
    pub candidate: Digest,
    /// The comparator's id.
    pub comparator: Digest,
    /// The admission set.
    pub set: SetId,
    /// The look.
    pub look: u8,
    /// The bit.
    pub bit: bool,
}

impl EvalRecord {
    /// The record of `figures`, a look of `set`.
    #[inline]
    #[must_use]
    pub const fn of(set: SetId, figures: &LookFigures) -> EvalRecord {
        EvalRecord {
            candidate: figures.candidate(),
            comparator: figures.comparator(),
            set,
            look: figures.look(),
            bit: figures.verdict.is_ok(),
        }
    }

    /// Its file's bytes.
    #[inline]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.raw(EVAL_DOMAIN);
        e.digest(&self.candidate);
        e.digest(&self.comparator);
        e.u8(self.set.tag.tag());
        e.u32(self.set.index);
        e.u8(self.look);
        e.bool(self.bit);
        e.finish()
    }
}

/// What a store transition wrote and read, as names and byte counts, for the cost report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moved {
    /// The files written.
    pub written: Vec<(String, usize)>,
    /// The files read to validate it.
    pub read: Vec<(String, usize)>,
}

/// One store (design §13.5): one build, one M6 policy and one pair, its versions, revisions and
/// pointer.
pub struct Store<S> {
    io: S,
    id: Digest,
    build: String,
    pair: Pair,
    pointer: Pointer,
    revisions: Vec<Revision>,
    versions: BTreeMap<Digest, Version>,
}

impl<S> fmt::Debug for Store<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("id", &self.id)
            .field("pair", &self.pair)
            .field("pointer", &self.pointer)
            .finish_non_exhaustive()
    }
}

#[inline]
fn store_text(build: &str, pair: Pair, nonce: u64) -> String {
    let pair_name = fmt::from_fn(move |f| match pair {
        Pair::Real(i) => write!(f, "pair-{i}"),
        Pair::Practice(j) => write!(f, "practice-{j}"),
    });
    format!(
        "{STORE_HEADER}\nm6-policy {}\nm4-policy {}\nbuild {build}\npair {pair_name}\nnonce {nonce:016x}\n",
        M6_POLICY_V1_DIGEST.hex(),
        RankerPolicy::v1().acceptance().hex(),
    )
}

#[inline]
fn version_dir(id: &Digest) -> String {
    format!("versions/{}", id.hex())
}

#[inline]
fn revision_name(n: u32) -> String {
    format!("revisions/{n:04}")
}

const VERSION_FILES: [&str; 4] = ["descriptor", "state", "manifest", "labels"];

impl<S: DurableStore> Store<S> {
    /// A new store for `pair` in `build`, with v0 active and at the head (revision 0).
    /// # Errors
    /// A write's failure.
    #[inline]
    pub fn init(
        mut io: S,
        build: &str,
        pair: Pair,
        nonce: u64,
        ledger: Digest,
    ) -> Result<Store<S>, StoreRefusal> {
        let text = store_text(build, pair, nonce);
        io.create(Step::StoreFile, "store.txt", text.as_bytes())?;
        let v0 = Version::base();
        let mut store = Store {
            io,
            id: Digest::of(STORE_DOMAIN, &[text.as_bytes()]),
            build: build.to_owned(),
            pair,
            pointer: Pointer {
                revision: 0,
                active: v0.id,
                head: v0.id,
            },
            revisions: Vec::new(),
            versions: BTreeMap::new(),
        };
        store.keep(&v0)?;
        store.revise(v0.id, v0.id, Reason::Init, None, ledger)?;
        Ok(store)
    }

    /// The store `io` holds, checked as it loads for `build` (design §13.5).
    /// # Errors
    /// [`StoreRefusal::PolicyMismatch`], [`StoreRefusal::ForeignBuild`],
    /// [`StoreRefusal::StoreCorrupt`] or [`StoreRefusal::StoreTorn`].
    #[inline]
    pub fn load(io: S, build: &str) -> Result<Store<S>, StoreRefusal> {
        let corrupt = |file: &str| StoreRefusal::StoreCorrupt {
            file: file.to_owned(),
        };
        let text = io.read("store.txt")?.ok_or_else(|| corrupt("store.txt"))?;
        let text = String::from_utf8(text).map_err(|_| corrupt("store.txt"))?;
        let fields: BTreeMap<&str, &str> = text
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(' '))
            .collect();
        if text.lines().next() != Some(STORE_HEADER) {
            return Err(corrupt("store.txt"));
        }
        if fields.get("m6-policy") != Some(&M6_POLICY_V1_DIGEST.hex().as_str())
            || fields.get("m4-policy") != Some(&RankerPolicy::v1().acceptance().hex().as_str())
        {
            return Err(StoreRefusal::PolicyMismatch);
        }
        let found = fields.get("build").copied().unwrap_or("");
        if found != build {
            return Err(StoreRefusal::ForeignBuild {
                found: found.to_owned(),
            });
        }
        let pair = fields.get("pair").copied().unwrap_or("");
        let nonce = fields
            .get("nonce")
            .and_then(|n| u64::from_str_radix(n, 16).ok());
        let pair = parse_pair(pair).ok_or_else(|| corrupt("store.txt"))?;
        if nonce.map(|n| store_text(build, pair, n)) != Some(text.clone()) {
            return Err(corrupt("store.txt"));
        }
        let mut versions = BTreeMap::new();
        for name in io.list("versions")? {
            let dir = format!("versions/{name}");
            let mut files = Vec::new();
            for file in VERSION_FILES {
                let path = format!("{dir}/{file}");
                files.push(io.read(&path)?.ok_or_else(|| corrupt(&path))?);
            }
            let v = Version::from_files(&files[0], &files[1], &files[2], &files[3])
                .map_err(|_| corrupt(&dir))?;
            if v.id.hex() != name {
                return Err(corrupt(&dir));
            }
            versions.insert(v.id, v);
        }
        let mut revisions = Vec::new();
        for (i, name) in io.list("revisions")?.iter().enumerate() {
            let path = format!("revisions/{name}");
            let bytes = io.read(&path)?.ok_or_else(|| corrupt(&path))?;
            let r = Revision::decode(&bytes).ok_or_else(|| corrupt(&path))?;
            if r.n as usize != i || revision_name(r.n) != path {
                return Err(corrupt(&path));
            }
            revisions.push(r);
        }
        let bytes = io.read("active")?.ok_or_else(|| corrupt("active"))?;
        let pointer = Pointer::decode(&bytes).ok_or_else(|| corrupt("active"))?;
        let last = revisions.last().ok_or(StoreRefusal::StoreTorn)?;
        if (last.n, last.active, last.head) != (pointer.revision, pointer.active, pointer.head) {
            return Err(StoreRefusal::StoreTorn);
        }
        if !versions.contains_key(&pointer.active) || !versions.contains_key(&pointer.head) {
            return Err(corrupt("active"));
        }
        Ok(Store {
            io,
            id: Digest::of(STORE_DOMAIN, &[text.as_bytes()]),
            build: build.to_owned(),
            pair,
            pointer,
            revisions,
            versions,
        })
    }

    /// Its id.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> Digest {
        self.id
    }

    /// The build it binds.
    #[inline]
    #[must_use]
    pub fn build(&self) -> &str {
        &self.build
    }

    /// The pair it binds.
    #[inline]
    #[must_use]
    pub const fn pair(&self) -> Pair {
        self.pair
    }

    /// Its pointer.
    #[inline]
    #[must_use]
    pub const fn pointer(&self) -> Pointer {
        self.pointer
    }

    /// Its revisions, in order.
    #[inline]
    #[must_use]
    pub fn revisions(&self) -> &[Revision] {
        &self.revisions
    }

    /// The version `id`, if retained.
    #[inline]
    #[must_use]
    pub fn version(&self, id: &Digest) -> Option<&Version> {
        self.versions.get(id)
    }

    /// Every retained version, by id.
    #[inline]
    #[must_use]
    pub const fn versions(&self) -> &BTreeMap<Digest, Version> {
        &self.versions
    }

    /// Its I/O.
    #[inline]
    #[must_use]
    pub const fn io(&self) -> &S {
        &self.io
    }

    /// Keeps `v` as a new version directory, unless it is retained already.
    #[inline]
    fn keep(&mut self, v: &Version) -> Result<Vec<(String, usize)>, StoreRefusal> {
        if let Some(kept) = self.versions.get(&v.id) {
            return if kept == v {
                Ok(Vec::new())
            } else {
                Err(StoreRefusal::StoreCorrupt {
                    file: version_dir(&v.id),
                })
            };
        }
        if self.versions.len() >= MAX_VERSIONS {
            return Err(StoreRefusal::Budget { what: "versions" });
        }
        let parts = [
            v.descriptor(),
            v.state.encode(),
            v.manifest.encode(),
            v.labels_bytes(),
        ];
        let bytes = parts[0].len() + parts[1].len() + parts[2].len() + parts[3].len();
        if bytes as u64 > MAX_VERSION_BYTES {
            return Err(StoreRefusal::Budget {
                what: "a version directory",
            });
        }
        let files: [(&str, &[u8]); 4] = [
            (VERSION_FILES[0], &parts[0]),
            (VERSION_FILES[1], &parts[1]),
            (VERSION_FILES[2], &parts[2]),
            (VERSION_FILES[3], &parts[3]),
        ];
        let dir = version_dir(&v.id);
        self.io.create_dir(Step::VersionFiles, &dir, &files)?;
        self.versions.insert(v.id, v.clone());
        let mut written = Vec::with_capacity(4);
        for (name, bytes) in files {
            written.push((format!("{dir}/{name}"), bytes.len()));
        }
        Ok(written)
    }

    /// Appends a revision, then moves the pointer to it: two writes, between which a load
    /// refuses [`StoreRefusal::StoreTorn`].
    #[inline]
    fn revise(
        &mut self,
        active: Digest,
        head: Digest,
        reason: Reason,
        look: Option<u8>,
        ledger: Digest,
    ) -> Result<Vec<(String, usize)>, StoreRefusal> {
        let n = len32(self.revisions.len());
        if self.revisions.len() >= MAX_REVISIONS {
            return Err(StoreRefusal::Budget {
                what: "RevisionBudget",
            });
        }
        let r = Revision {
            n,
            active,
            head,
            reason,
            look,
            ledger,
        };
        let line = r.encode();
        if line.len() as u64 > MAX_LINE_BYTES {
            return Err(StoreRefusal::Budget { what: "a revision" });
        }
        let pointer = Pointer {
            revision: n,
            active,
            head,
        };
        let name = revision_name(n);
        self.io.create(Step::Revision, &name, &line)?;
        self.revisions.push(r);
        let bytes = pointer.encode();
        self.io.replace(Step::Pointer, "active", &bytes)?;
        self.pointer = pointer;
        Ok(vec![(name, line.len()), ("active".to_owned(), bytes.len())])
    }

    /// Trains the member after the head (round head + 1, up to [`ROUNDS`]) and moves the head to
    /// it, writing its directory only when it is not retained; the active id never changes.
    /// # Errors
    /// [`LearnRefusal::Round`] past round 4, the learner's, a budget's or a write's.
    #[inline]
    pub fn train(
        &mut self,
        sets: &Sets,
        labeller: &Labeller<'_>,
        ledger: Digest,
    ) -> Result<(Version, Moved), StoreRefusal> {
        let head = self
            .versions
            .get(&self.pointer.head)
            .ok_or(StoreRefusal::NotRetained)?
            .clone();
        let next = derive(&head, sets, labeller)?;
        let mut moved = Moved {
            written: self.keep(&next)?,
            read: Vec::new(),
        };
        moved.written.extend(self.revise(
            self.pointer.active,
            next.id,
            Reason::Train,
            None,
            ledger,
        )?);
        Ok((next, moved))
    }

    /// Publishes an admission (design §13.5): the pointer moves to its candidate in one rename,
    /// with an `admit` revision naming its look.
    /// # Errors
    /// [`StoreRefusal::Control`], [`StoreRefusal::ForeignPair`], [`StoreRefusal::Republished`],
    /// [`StoreRefusal::NotComparator`], [`StoreRefusal::NotRetained`] or a write's.
    #[inline]
    pub fn activate(
        &mut self,
        admitted: ModelAdmitted,
        ledger: Digest,
    ) -> Result<Moved, StoreRefusal> {
        let ModelAdmitted {
            pair,
            look,
            candidate,
            comparator,
        } = admitted;
        if candidate == control_id() {
            return Err(StoreRefusal::Control);
        }
        if pair != self.pair {
            return Err(StoreRefusal::ForeignPair);
        }
        for r in &self.revisions {
            if r.reason == Reason::Admit && r.look == Some(look) {
                return Err(StoreRefusal::Republished { look });
            }
        }
        if self.pointer.active != comparator {
            return Err(StoreRefusal::NotComparator);
        }
        if !self.versions.contains_key(&candidate) {
            return Err(StoreRefusal::NotRetained);
        }
        Ok(Moved {
            written: self.revise(
                candidate,
                self.pointer.head,
                Reason::Admit,
                Some(look),
                ledger,
            )?,
            read: Vec::new(),
        })
    }

    /// Rolls back to `to` (design §13.5): a retained version whose files hash to its id, and v0
    /// or a member an `admit` revision published. The pointer and head move to it in one
    /// rename, with a `rollback` revision; the ledger is never touched.
    /// # Errors
    /// [`StoreRefusal::NotRetained`], [`StoreRefusal::StoreCorrupt`],
    /// [`StoreRefusal::NotAdmitted`] or a write's.
    #[inline]
    pub fn rollback(&mut self, to: Digest, ledger: Digest) -> Result<Moved, StoreRefusal> {
        let kept = self.versions.get(&to).ok_or(StoreRefusal::NotRetained)?;
        let dir = version_dir(&to);
        let mut read = Vec::new();
        let mut files = Vec::new();
        for file in VERSION_FILES {
            let path = format!("{dir}/{file}");
            let bytes = self
                .io
                .read(&path)?
                .ok_or_else(|| StoreRefusal::StoreCorrupt { file: path.clone() })?;
            read.push((path, bytes.len()));
            files.push(bytes);
        }
        let reread = Version::from_files(&files[0], &files[1], &files[2], &files[3])
            .map_err(|_| StoreRefusal::StoreCorrupt { file: dir.clone() })?;
        if reread.id != to || reread != *kept {
            return Err(StoreRefusal::StoreCorrupt { file: dir });
        }
        let mut admitted = false;
        for r in &self.revisions {
            if r.reason == Reason::Admit && r.active == to {
                admitted = true;
                break;
            }
        }
        if to != Version::base().id && !admitted {
            return Err(StoreRefusal::NotAdmitted);
        }
        Ok(Moved {
            written: self.revise(to, to, Reason::Rollback, None, ledger)?,
            read,
        })
    }

    /// Keeps `v` as [`Self::train`] keeps a member (tests: the version budget).
    #[cfg(test)]
    pub(super) fn keep_version(&mut self, v: &Version) -> Result<(), StoreRefusal> {
        self.keep(v).map(|_| ())
    }

    /// Writes look `look`'s eval record, once: a look of this store's pair.
    /// # Errors
    /// [`StoreRefusal::ForeignPair`], a budget's or a write's.
    #[inline]
    pub fn write_eval(&mut self, record: &EvalRecord) -> Result<(), StoreRefusal> {
        if record.set != self.pair.looks() {
            return Err(StoreRefusal::ForeignPair);
        }
        let bytes = record.encode();
        if record.look == 0 || record.look > LOOKS || bytes.len() as u64 > MAX_LINE_BYTES {
            return Err(StoreRefusal::Budget {
                what: "an eval record",
            });
        }
        self.io
            .create(Step::Eval, &format!("eval/{}.rec", record.look), &bytes)?;
        Ok(())
    }

    /// Writes a look's figures file, once, after the report: a look of this store's pair.
    /// # Errors
    /// [`StoreRefusal::ForeignPair`], a budget's or a write's.
    #[inline]
    pub fn write_figures(&mut self, figures: &LookFigures) -> Result<(), StoreRefusal> {
        if figures.pair() != self.pair {
            return Err(StoreRefusal::ForeignPair);
        }
        let bytes = figures.encode();
        let look = figures.look();
        if look == 0 || look > LOOKS || bytes.len() as u64 > MAX_FIGURES_BYTES {
            return Err(StoreRefusal::Budget {
                what: "a figures file",
            });
        }
        self.io
            .create(Step::Figures, &format!("eval/{look}.figures"), &bytes)?;
        Ok(())
    }

    /// Writes the report, once, and returns its digest for `report-done`.
    /// # Errors
    /// A budget's or a write's.
    #[inline]
    pub fn write_report(&mut self, report: &Report) -> Result<Digest, StoreRefusal> {
        let text = report.text();
        if text.len() as u64 > MAX_REPORT_BYTES {
            return Err(StoreRefusal::Budget { what: "the report" });
        }
        self.io
            .create(Step::Report, "report.txt", text.as_bytes())?;
        Ok(Digest::of(REPORT_DOMAIN, &[text.as_bytes()]))
    }
}

/// The pair a `store.txt` names.
#[inline]
fn parse_pair(name: &str) -> Option<Pair> {
    if let Some(i) = name.strip_prefix("pair-") {
        return i.parse().ok().map(Pair::Real);
    }
    name.strip_prefix("practice-")?
        .parse()
        .ok()
        .map(Pair::Practice)
}

/// M4's training record of the grid (design §13.9, G1), which a full session's labels of
/// [`GRID_V1`] must reproduce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridRecord {
    /// Plans judged.
    pub judged: usize,
    /// Plans admitted on every training item.
    pub admitted: usize,
    /// Refused plans by their first failing law, in law order.
    pub refused: Vec<(&'static str, usize)>,
    /// The best admitted plan's bytes, as hex, and its training `ΣT` and `ΣZ`.
    pub best: Option<(String, u64, u64)>,
}

impl GridRecord {
    /// M4's record: 54 judged and 18 admitted; refused `HotHasWake` 9, `NoStepSlept` 9 and
    /// `WakeNoLaterThanClear` 18; best `010100000000000000000000` at `ΣT` 6,394 and `ΣZ` 94.
    #[inline]
    #[must_use]
    pub fn m4() -> GridRecord {
        GridRecord {
            judged: 54,
            admitted: 18,
            refused: vec![
                ("HotHasWake", 9),
                ("NoStepSlept", 9),
                ("WakeNoLaterThanClear", 18),
            ],
            best: Some(("010100000000000000000000".to_owned(), 6_394, 94)),
        }
    }

    /// The record of `labels`.
    #[inline]
    #[must_use]
    pub fn of(labels: &[TrainLabel]) -> GridRecord {
        let mut refused: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut best: Option<(u64, u64, [u8; 12])> = None;
        let mut admitted = 0usize;
        for l in labels {
            match (l.totals(), l.law()) {
                (Some(t), _) => {
                    admitted += 1;
                    let key = (t.ticks, t.wasted, l.plan().encode());
                    if best.is_none_or(|k| key < k) {
                        best = Some(key);
                    }
                }
                (None, law) => *refused.entry(law.unwrap_or("")).or_default() += 1,
            }
        }
        GridRecord {
            judged: labels.len(),
            admitted,
            refused: refused.into_iter().collect(),
            best: best.map(|(t, z, bytes)| {
                let mut s = String::with_capacity(24);
                for b in bytes {
                    let _ = write!(s, "{b:02x}");
                }
                (s, t, z)
            }),
        }
    }
}

/// Why a session stopped or failed, by the step it reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRefusal {
    /// G1: the M6 policy, its sets, the chain or the grid's record does not reproduce. The
    /// session stops before its first look, and its pair is not spent.
    NotReproduced {
        /// What did not.
        what: &'static str,
    },
    /// The ledger refused.
    Ledger(LedgerRefusal),
    /// The store refused.
    Store(StoreRefusal),
    /// A rollback's retrain did not reproduce its retained successor (C4).
    Rollback {
        /// The round that did not.
        round: u8,
    },
}

impl fmt::Display for SessionRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionRefusal::NotReproduced { what } => {
                write!(f, "session stopped: NotReproduced, {what}")
            }
            SessionRefusal::Ledger(e) => write!(f, "session: {e}"),
            SessionRefusal::Store(e) => write!(f, "session: {e}"),
            SessionRefusal::Rollback { round } => write!(
                f,
                "session defect: a rollback's retrain of round {round} did not reproduce it"
            ),
        }
    }
}

impl std::error::Error for SessionRefusal {}

impl From<LedgerRefusal> for SessionRefusal {
    fn from(e: LedgerRefusal) -> SessionRefusal {
        SessionRefusal::Ledger(e)
    }
}

impl From<StoreRefusal> for SessionRefusal {
    fn from(e: StoreRefusal) -> SessionRefusal {
        SessionRefusal::Store(e)
    }
}

impl From<LearnRefusal> for SessionRefusal {
    fn from(e: LearnRefusal) -> SessionRefusal {
        SessionRefusal::Store(StoreRefusal::Learn(e))
    }
}

/// What a session ran on.
#[derive(Clone, Copy)]
pub struct SessionEnv<'a> {
    /// The build.
    pub build: &'a str,
    /// The sets.
    pub sets: &'a Sets,
    /// The labeller.
    pub labeller: Labeller<'a>,
    /// The chain the session must reproduce: [`M6_CHAIN_V1`] in the lab, a test's own in tests.
    pub chain: ChainPin,
    /// The tests log's digest, which the `open` line records.
    pub tests_log: Digest,
}

impl fmt::Debug for SessionEnv<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionEnv")
            .field("build", &self.build)
            .field("chain", &self.chain)
            .finish_non_exhaustive()
    }
}

/// What a finished child did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRun {
    /// The chain v0..v4, as G1 trained it.
    pub chain: Vec<Version>,
    /// Each look's bit, in order: recorded by child A, replayed by child B.
    pub bits: Vec<bool>,
    /// The active version at report time (Mf).
    pub mf: Digest,
    /// The report, child A's alone.
    pub report: Option<Report>,
    /// Each look's figures, as replayed.
    pub figures: Vec<LookFigures>,
    /// Whether R2 was exercised: the round rolled back to.
    pub r2: Option<u8>,
    /// Every store transition's writes and reads, in order, for the cost report.
    pub moves: Vec<(&'static str, Moved)>,
}

/// Before step 1: the store binds the ledger's pair and the session's build, so a session on
/// another pair's store stops before it writes a line.
#[inline]
fn binds<L: DurableLog, S: DurableStore>(
    env: &SessionEnv<'_>,
    ledger: &Ledger<L>,
    store: &Store<S>,
) -> Result<(), StoreRefusal> {
    if store.pair() != ledger.pair() {
        return Err(StoreRefusal::ForeignPair);
    }
    if store.build() != env.build {
        return Err(StoreRefusal::ForeignBuild {
            found: store.build().to_owned(),
        });
    }
    Ok(())
}

/// Step 1 (design §13.9): G1, then the chain trained into the store, compared with the pin.
#[inline]
fn reproduce<S: DurableStore>(
    env: &SessionEnv<'_>,
    store: &mut Store<S>,
    ledger: Digest,
    moves: &mut Vec<(&'static str, Moved)>,
) -> Result<Vec<Version>, SessionRefusal> {
    let not = |what| SessionRefusal::NotReproduced { what };
    RankerPolicy::pinned().map_err(|_| not("the M6 policy or its sets"))?;
    let mut chain = vec![
        store
            .version(&store.pointer().head)
            .ok_or(StoreRefusal::NotRetained)?
            .clone(),
    ];
    for _ in 0..ROUNDS {
        let (v, moved) = store.train(env.sets, &env.labeller, ledger)?;
        moves.push(("train", moved));
        chain.push(v);
    }
    if ChainPin::of(&chain) != Some(env.chain) {
        return Err(not("the chain"));
    }
    if env.sets.full() {
        let grid = env
            .labeller
            .label(LabelScope::Train, &bytes_of(&env.sets.grid()))
            .map_err(|e| SessionRefusal::Store(StoreRefusal::Learn(e.into())))?;
        if GridRecord::of(&grid) != GridRecord::m4() {
            return Err(not("the grid's training record"));
        }
    }
    Ok(chain)
}

/// Steps 7 to 9: R1, R2, and Mf restored, each retrain checked against the chain.
#[inline]
fn roll<S: DurableStore>(
    env: &SessionEnv<'_>,
    store: &mut Store<S>,
    ledger: Digest,
    chain: &[Version],
    mf: Digest,
    moves: &mut Vec<(&'static str, Moved)>,
) -> Result<Option<u8>, SessionRefusal> {
    let retrain = |store: &mut Store<S>, moves: &mut Vec<(&'static str, Moved)>, round: u8| {
        let (v, moved) = store.train(env.sets, &env.labeller, ledger)?;
        let mut wrote_version = false;
        for (n, _) in &moved.written {
            if n.starts_with("versions/") {
                wrote_version = true;
                break;
            }
        }
        moves.push(("retrain", moved));
        if Some(&v) != chain.get(usize::from(round)) || wrote_version {
            return Err(SessionRefusal::Rollback { round });
        }
        Ok(())
    };
    moves.push(("rollback", store.rollback(chain[0].id, ledger)?));
    retrain(store, moves, 1)?;
    retrain(store, moves, 2)?;
    let mut r2 = None;
    for v in chain[1..=3].iter().rev() {
        let mut was_admitted = false;
        for r in store.revisions() {
            if r.reason == Reason::Admit && r.active == v.id {
                was_admitted = true;
                break;
            }
        }
        if was_admitted {
            r2 = Some(v.round);
            break;
        }
    }
    if let Some(round) = r2 {
        moves.push((
            "rollback",
            store.rollback(chain[usize::from(round)].id, ledger)?,
        ));
        retrain(store, moves, round + 1)?;
    }
    moves.push(("rollback", store.rollback(mf, ledger)?));
    Ok(r2)
}

/// Child A (design §13.9): reproduce the chain, take the five looks in order, publishing each
/// admission, read the report once, replay and retire the looks, then R1, R2 and Mf restored.
/// # Errors
/// The first refusal; before the `open` line the session is stopped.
#[inline]
pub fn session_a<L: DurableLog, S: DurableStore>(
    env: &SessionEnv<'_>,
    ledger: &mut Ledger<L>,
    store: &mut Store<S>,
) -> Result<SessionRun, SessionRefusal> {
    let mut moves = Vec::new();
    binds(env, ledger, store)?;
    let chain = reproduce(env, store, ledger.head(), &mut moves)?;
    ledger.open(store.id(), env.build, env.tests_log, false)?;
    let set = ledger.pair().looks();
    for n in 1..=LOOKS {
        let (x, a) =
            look_pair(n, &ledger.bits(), &env.chain.ids).ok_or(LedgerRefusal::OutOfOrder)?;
        let look = ledger.begin_look(n, x, a, &chain, env.sets)?;
        let figures = run_look(&env.labeller, look)?;
        let admitted = ledger.verdict(&figures)?;
        store.write_eval(&EvalRecord::of(set, &figures))?;
        if let Some(admitted) = admitted {
            moves.push(("activate", store.activate(admitted, ledger.head())?));
        }
    }
    let mf = store.pointer().active;
    let read = ledger.begin_report(env.sets)?;
    let report = run_report(&env.labeller, read, ledger.pair(), &chain, &mf)?;
    let digest = store.write_report(&report)?;
    ledger.report_done(digest)?;
    let mut figures = Vec::new();
    for n in 1..=LOOKS {
        let replay = ledger.begin_replay(n, &chain, env.sets)?;
        let (_published_already, replayed) = ledger.replayed(run_replay(&env.labeller, replay)?)?;
        store.write_figures(&replayed)?;
        figures.push(replayed);
    }
    ledger.retire()?;
    let r2 = roll(env, store, ledger.head(), &chain, mf, &mut moves)?;
    Ok(SessionRun {
        chain,
        bits: ledger.bits(),
        mf,
        report: Some(report),
        figures,
        r2,
        moves,
    })
}

/// Child B (design §13.9): reproduce the chain in its own store, open flagged `replay`, replay
/// each look under the ledger's replay tokens, publishing by replayed admission and writing each
/// look's eval record and figures, then R1, R2 and Mf restored. It reads no report and writes
/// no `retire`.
/// # Errors
/// The first refusal, [`LedgerRefusal::ReplayMismatch`] (a defect) among them.
#[inline]
pub fn session_b<L: DurableLog, S: DurableStore>(
    env: &SessionEnv<'_>,
    ledger: &mut Ledger<L>,
    store: &mut Store<S>,
) -> Result<SessionRun, SessionRefusal> {
    let mut moves = Vec::new();
    binds(env, ledger, store)?;
    let chain = reproduce(env, store, ledger.head(), &mut moves)?;
    ledger.open(store.id(), env.build, env.tests_log, true)?;
    let set = ledger.pair().looks();
    let mut figures = Vec::new();
    let mut bits = Vec::new();
    let mut mf = store.pointer().active;
    for n in 1..=LOOKS {
        let replay = ledger.begin_replay(n, &chain, env.sets)?;
        let (admitted, replayed) = ledger.replayed(run_replay(&env.labeller, replay)?)?;
        store.write_eval(&EvalRecord::of(set, &replayed))?;
        if let Some(admitted) = admitted {
            moves.push(("activate", store.activate(admitted, ledger.head())?));
        }
        store.write_figures(&replayed)?;
        bits.push(replayed.bit());
        mf = store.pointer().active;
        figures.push(replayed);
    }
    let r2 = roll(env, store, ledger.head(), &chain, mf, &mut moves)?;
    Ok(SessionRun {
        chain,
        bits,
        mf,
        report: None,
        figures,
        r2,
        moves,
    })
}
