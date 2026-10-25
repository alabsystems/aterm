// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `momentum-glow.m6-ranker-admission` v1, the Aos M6 model-admission policy (design §13), pinned before any learner code.

use core::fmt;

use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, ArtifactKind, Digest};

use super::corpus::SplitMix;
use super::plan::{
    MAX_GAP_US, MAX_PERIOD_US, MIN_PERIOD_US, PlanRefusal, SchedulePlan, Split, Wake,
};
use super::policy::{AcceptancePolicy, GRID_V1, M4_POLICY_V1_DIGEST, hex_digest};

/// The policy artifact's name.
pub const RANKER_POLICY_NAME: &str = "momentum-glow.m6-ranker-admission";

/// The policy artifact's version.
pub const RANKER_POLICY_VERSION: u32 = 1;

/// The digest of [`RankerPolicy::v1`]'s artifact, pinned, which only a reviewed source commit moves.
pub const M6_POLICY_V1_DIGEST: Digest =
    hex_digest("b234b312c1179525507ccbd2e75fe59ecef3dfafa0b8269f92bf20def3355e41");

/// The digest of every set v1 draws ([`sets_digest`]), pinned.
pub const M6_SETS_V1_DIGEST: Digest =
    hex_digest("2f2bf77b95d81ea833e45bb59406765c4016b7c1df25f04528dbb9b82c5ffc2e");

/// The model's weights: 22 split classes, 15 wake classes and their 330 crosses.
pub const FEATURES: usize = 367;

/// The plans drawn into one pool (N).
pub const POOL_PLANS: usize = 64;

/// The plans of a pool a ranked arm judges (K).
pub const JUDGED: usize = 4;

/// The pools of a held-out, report or practice set (P).
pub const POOLS: usize = 64;

/// The pools of the train set: `T_0..T_3` for the cold start, `T_4..T_6` for chain evidence.
pub const TRAIN_POOLS: usize = 7;

/// The lowest-tie-key plans a round adds beside its parent's top K (X).
pub const EXPLORE: usize = 12;

/// The epochs of one training run.
pub const EPOCHS: u32 = 16;

/// The highest round: v1 is the cold start (round 1) and v2, v3 and v4 are one online round each, so the chain is v0..=v4.
pub const ROUNDS: u8 = 4;

/// The most trainer steps across a warm-started chain.
pub const MAX_STEPS: u64 = (1 << 24) - 1;

/// The most draws a pool may take to fill.
pub const MAX_DRAWS: u32 = 16_384;

/// A plan is high-mass when one draw yields it with probability above `2^-MASS_CAP_LOG2`.
pub const MASS_CAP_LOG2: u32 = 16;

/// The most labelled plans in one version's data.
pub const MAX_LABELS: usize = 1_024;

/// The most distinct plans one process labels.
pub const MAX_JUDGED: usize = 16_384;

/// The looks per held-out set.
pub const LOOKS: u8 = 5;

/// A look's sign test is at level `1 / LOOK_LEVEL`: 0.05 over the 31 nodes of the look tree.
pub const LOOK_LEVEL: u128 = 620;

/// A report criterion's sign test is at level `1 / REPORT_LEVEL`: 0.05 over three criteria.
pub const REPORT_LEVEL: u128 = 60;

/// The most versions a store retains.
pub const MAX_VERSIONS: usize = 16;

/// The most bytes of one version's directory.
pub const MAX_VERSION_BYTES: u64 = 64 << 10;

/// The most revisions a store holds.
pub const MAX_REVISIONS: usize = 64;

/// The most bytes of one revision line or eval record.
pub const MAX_LINE_BYTES: u64 = 256;

/// The most bytes of one look's figures file.
pub const MAX_FIGURES_BYTES: u64 = 16 << 10;

/// The most bytes of the report.
pub const MAX_REPORT_BYTES: u64 = 64 << 10;

/// The wall time each session child may take before its runner kills it, in seconds.
pub const CHILD_DEADLINE_S: u64 = 3_600;

/// The most lifetime-maximum `phys_footprint` a session child may reach, in bytes.
pub const MAX_FOOTPRINT: u64 = 2 << 30;

/// The longest plan list a look or replay token binds: two arms' top K over every pool.
pub const LOOK_LIST_CAP: usize = 2 * JUDGED * POOLS;

/// The longest plan list a read token binds: every plan of every pool.
pub const READ_LIST_CAP: usize = POOL_PLANS * POOLS;

/// The rule that names a plan's features.
pub const FEATURES_RULE: &str = "m6-features v1: split class s is 0 for Shipped, 1 for \
     SettleAtStep with gap 0, and 33 - leading_zeros(g) for a gap g >= 1; wake class w is 0 \
     NextStep, 1 ColdOnly, 2 Cadence, 3 Never, and floor(log2 p) - 5 for Poll p; the active \
     features are s, 22 + w and 37 + 15 s + w";

/// The rule that orders a pool.
pub const RANK_RULE: &str = "m6-rank v1: score = sum over active features of t B[f] + acc[f] \
     in i64; score descending, then the tie key ascending; v0 scores every plan 0";

/// The rule that trains one chain member.
pub const TRAIN_RULE: &str = "m6-train v1: warm start from the parent's state; EPOCHS epochs \
     over the data in manifest order, each permuted by Fisher-Yates with SplitMix::below; for \
     each consecutive pair whose label order differs, if the better plan's w-score is not above \
     the worse's, w += 1 on its features and -= 1 on the worse's; then t += 1 and acc += w; \
     label order: admitted before refused, admitted by training (sum T, sum Z) ascending, \
     refused plans tie";

/// The rule that picks a round's data.
pub const DATA_RULE: &str = "m6-data v1: D1 = Grid then T_0..T_3; D_r = D_(r-1) then \
     ChainEvidence(r) for r = 2..=4: the parent's top K on T_(2+r), then the X other plans of \
     that pool with the lowest tie keys; every D_r deduplicated by candidate id, keeping first \
     occurrence";

/// The control ranker.
pub const CONTROL_RULE: &str = "m6-control v1: each pool ordered by the first 8 bytes LE of \
     the control digest, then the tie key; no state, never trained, published or rolled back";

/// The sampler.
pub const POOLS_RULE: &str = "m6-pools v1: split = below(2); for SettleAtStep a gap bucket \
     b = below(21), gap 0 when b = 0, else lo + below(hi - lo + 1) with lo = 2^(b-1), \
     hi = min(2^b - 1, MAX_GAP_US); wake = below(5) in tag order; for Poll a bucket \
     c = 9 + below(11), then lo + below(hi - lo + 1) with lo = max(MIN_PERIOD_US, 2^c), \
     hi = min(2^(c+1) - 1, MAX_PERIOD_US); a pool is the first N distinct plans its set accepts";

/// The side rule, which no M6 policy version changes.
pub const SIDE_RULE: &str = "m6-side v1: side = Digest::of(side domain, [plan bytes])[0] mod 4: \
     0 train, 1 held-out, 2 report, 3 practice";

/// What a held-out, report or practice set skips.
pub const SKIP_RULE: &str = "m6-skips v1: every GRID_V1 plan, the M4 plans file's decoding \
     lines, every plan above 2^-MASS_CAP_LOG2 per draw, every plan of an earlier version's \
     opened real sets, and for practice-report j every plan of practice-held-out j";

/// The rule that admits a version.
pub const ADMISSION_RULE: &str = "m6-admission v1: rank each pool by candidate and comparator, \
     take each one's top K, label the union under the look's token, fold each pool, compare by \
     (class, sum T, sum Z) lower better; w wins, l losses, n = w + l; admit iff n >= 1 and \
     LOOK_LEVEL * sum_(k=w..n) C(n, k) <= 2^n exactly, and the candidate's summed gains in T and \
     Z are each not lower than the comparator's";

/// The order of the looks.
pub const LOOK_ORDER_RULE: &str = "m6-looks v1: look 1 v1 against v0; look 2 the control \
     against v0, moving no pointer; looks 3, 4 and 5 v2, v3 and v4 against v0 replaced by each \
     candidate looks 1, 3 and 4 admitted, in turn";

/// The leakage and overfitting policy.
pub const LEAKAGE_RULE: &str = "m6-leakage v1: every learning choice is a term; the chain is \
     pinned before the rehearsal; looks are bit-only, capped and ordered; the ledger is written \
     ahead; E is read once; one session per pair; claims are about the report population only";

/// How every M6 digest encodes its parts.
pub const ENCODING_RULE: &str = "m6-encodings v1: the policy name as UTF-8, the version, set \
     index and pool index each a u32 LE, a set's tag one byte (0 train, 1 held-out, 2 report, \
     3 practice-held-out, 4 practice-report), a plan its 12 bytes; a key or seed is \
     u64::from_le_bytes of a digest's first 8 bytes";

/// The comparator W1.
pub const W1_RULE: &str = "m6-w1 v1: NextStep or Cadence first, then the tie key";

/// The comparator W2.
pub const W2_RULE: &str = "m6-w2 v1: SettleAtStep and NextStep first, then W1's order";

/// The domain of a pool's seed.
pub const POOL_DOMAIN: &[u8] = b"aterm-aos-m6-pool\0";
/// The domain of the side rule.
pub const SIDE_DOMAIN: &[u8] = b"aterm-aos-m6-side-v1\0";
/// The domain of the tie key.
pub const TIE_DOMAIN: &[u8] = b"aterm-aos-m6-tie\0";
/// The domain of the control's key.
pub const CONTROL_DOMAIN: &[u8] = b"aterm-aos-m6-control\0";
/// The domain of the control's id.
pub const CONTROL_ID_DOMAIN: &[u8] = b"aterm-aos-m6-control-id\0";
/// The domain of the trainer's seed.
pub const TRAINER_DOMAIN: &[u8] = b"aterm-aos-m6-trainer\0";
/// The domain of a model version's id.
pub const VERSION_DOMAIN: &[u8] = b"aterm-aos-m6-model-version-v1\0";
/// The domain of the base's digest.
pub const BASE_DOMAIN: &[u8] = b"aterm-aos-m6-base-v1\0";
/// The domain of [`sets_digest`].
pub const SETS_DOMAIN: &[u8] = b"aterm-aos-m6-sets-v1\0";

/// The M4 plans file's lines that decode (the record's table, lines 0 to 7), whose labels the designer has read.
pub const PLANS_FILE_V1: [(Split, Wake); 8] = [
    (Split::SettleAtStep { gap_us: 4_000 }, Wake::NextStep),
    (Split::SettleAtStep { gap_us: 6_000 }, Wake::NextStep),
    (Split::SettleAtStep { gap_us: 12_000 }, Wake::NextStep),
    (Split::SettleAtStep { gap_us: 1_000_000 }, Wake::NextStep),
    (Split::Shipped, Wake::NextStep),
    (
        Split::SettleAtStep { gap_us: 0 },
        Wake::Poll { period_us: 1_000 },
    ),
    (Split::SettleAtStep { gap_us: 0 }, Wake::Cadence),
    (Split::SettleAtStep { gap_us: 0 }, Wake::NextStep),
];

/// Which plans a set may hold, by [`side`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Side 0.
    Train,
    /// Side 1.
    HeldOut,
    /// Side 2.
    Report,
    /// Side 3, for the rehearsal and tests.
    Practice,
}

/// A plan's side: the first byte of its side digest, mod 4. No policy version changes it.
#[must_use]
pub fn side(plan: &SchedulePlan) -> Side {
    match Digest::of(SIDE_DOMAIN, &[&plan.encode()]).0[0] % 4 {
        0 => Side::Train,
        1 => Side::HeldOut,
        2 => Side::Report,
        _ => Side::Practice,
    }
}

/// A set's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SetTag {
    /// The train set, index 0.
    Train,
    /// A model-admission set `H_i`.
    HeldOut,
    /// A report set `E_i`.
    Report,
    /// A rehearsal's admission set.
    PracticeHeldOut,
    /// A rehearsal's report set.
    PracticeReport,
}

impl SetTag {
    /// Its byte in seeds and pool ids.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            SetTag::Train => 0,
            SetTag::HeldOut => 1,
            SetTag::Report => 2,
            SetTag::PracticeHeldOut => 3,
            SetTag::PracticeReport => 4,
        }
    }

    /// The side its plans must have.
    #[must_use]
    pub const fn side(self) -> Side {
        match self {
            SetTag::Train => Side::Train,
            SetTag::HeldOut => Side::HeldOut,
            SetTag::Report => Side::Report,
            SetTag::PracticeHeldOut | SetTag::PracticeReport => Side::Practice,
        }
    }

    /// Whether it is a real pair's set, which only the real sets' capability opens.
    #[must_use]
    pub const fn real(self) -> bool {
        matches!(self, SetTag::HeldOut | SetTag::Report)
    }
}

/// A set: its kind and index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SetId {
    /// Its kind.
    pub tag: SetTag,
    /// `i` for a real set, `j` for a practice set, 0 for the train set.
    pub index: u32,
}

/// One pool of a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolId {
    /// Its set.
    pub set: SetId,
    /// Its index in the set.
    pub pool: u32,
}

impl PoolId {
    const fn parts(&self) -> [[u8; 4]; 2] {
        [self.set.index.to_le_bytes(), self.pool.to_le_bytes()]
    }
}

const fn first_u64(d: &Digest) -> u64 {
    let b = d.0;
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn keyed(domain: &[u8], pool: &PoolId, plan: &SchedulePlan) -> u64 {
    let [i, p] = pool.parts();
    first_u64(&Digest::of(
        domain,
        &[
            RANKER_POLICY_NAME.as_bytes(),
            &RANKER_POLICY_VERSION.to_le_bytes(),
            &[pool.set.tag.tag()],
            &i,
            &p,
            &plan.encode(),
        ],
    ))
}

/// `plan`'s class under `m6-w1 v1` ([`W1_RULE`]): 0 for `NextStep` or `Cadence`, else 1.
#[must_use]
pub const fn w1_class(plan: &SchedulePlan) -> u8 {
    match plan.wake() {
        Wake::NextStep | Wake::Cadence => 0,
        _ => 1,
    }
}

/// `plan`'s class under `m6-w2 v1` ([`W2_RULE`]): 0 for `SettleAtStep ∧ NextStep`, else 1 plus
/// its W1 class.
#[must_use]
pub const fn w2_class(plan: &SchedulePlan) -> u8 {
    match (plan.split(), plan.wake()) {
        (Split::SettleAtStep { .. }, Wake::NextStep) => 0,
        _ => 1 + w1_class(plan),
    }
}

/// The tie key of `plan` in `pool`, ascending in every ranking.
#[must_use]
pub fn tie_key(pool: &PoolId, plan: &SchedulePlan) -> u64 {
    keyed(TIE_DOMAIN, pool, plan)
}

/// The control's key of `plan` in `pool`, ascending, before the tie key.
#[must_use]
pub fn control_key(pool: &PoolId, plan: &SchedulePlan) -> u64 {
    keyed(CONTROL_DOMAIN, pool, plan)
}

fn named(domain: &[u8]) -> Digest {
    Digest::of(
        domain,
        &[
            RANKER_POLICY_NAME.as_bytes(),
            &RANKER_POLICY_VERSION.to_le_bytes(),
        ],
    )
}

/// The control's id, which is never a version's.
#[must_use]
pub fn control_id() -> Digest {
    named(CONTROL_ID_DOMAIN)
}

/// The trainer's starting random state.
#[must_use]
pub fn trainer_seed() -> u64 {
    first_u64(&named(TRAINER_DOMAIN))
}

/// The digest of the frozen all-zero base table.
#[must_use]
pub fn base_digest() -> Digest {
    let mut e = Enc::new();
    e.u64(wide(FEATURES));
    for _ in 0..FEATURES {
        e.i32(0);
    }
    Digest::of(BASE_DOMAIN, &[&e.finish()])
}

/// The three distinct active features of `plan`, in `0..FEATURES`.
#[must_use]
pub const fn features(plan: &SchedulePlan) -> [usize; 3] {
    let s = match plan.split() {
        Split::Shipped => 0,
        Split::SettleAtStep { gap_us: 0 } => 1,
        Split::SettleAtStep { gap_us } => 33 - gap_us.leading_zeros() as usize,
    };
    let w = match plan.wake() {
        Wake::NextStep => 0,
        Wake::ColdOnly => 1,
        Wake::Cadence => 2,
        Wake::Never => 3,
        Wake::Poll { period_us } => 26 - period_us.leading_zeros() as usize,
    };
    [s, 22 + w, 37 + 15 * s + w]
}

/// The split choices of a draw: `Shipped`, then `SettleAtStep`.
const SPLITS: u64 = 2;

/// The gap choices of a `SettleAtStep` draw: gap 0, then gap buckets 1 to 20.
const GAP_BUCKETS: u64 = 21;

/// The wake choices of a draw, in tag order.
const WAKES: u64 = 5;

/// The period buckets of a `Poll` draw, from [`FIRST_PERIOD_BUCKET`].
const PERIOD_BUCKETS: u64 = 11;

/// The first period bucket, `2^9..2^10`, which starts at [`MIN_PERIOD_US`].
const FIRST_PERIOD_BUCKET: u32 = 9;

/// `[lo, hi]` of gap bucket `b` in `1..=20`.
const fn gap_bucket(b: u32) -> (u32, u32) {
    let lo = 1 << (b - 1);
    let hi = (1u32 << b) - 1;
    (lo, if hi < MAX_GAP_US { hi } else { MAX_GAP_US })
}

/// `[lo, hi]` of period bucket `c` in `9..=19`.
const fn period_bucket(c: u32) -> (u32, u32) {
    let lo = 1 << c;
    let hi = (1u32 << (c + 1)) - 1;
    (
        if lo > MIN_PERIOD_US {
            lo
        } else {
            MIN_PERIOD_US
        },
        if hi < MAX_PERIOD_US {
            hi
        } else {
            MAX_PERIOD_US
        },
    )
}

/// A choice in `0..m`, for `m` in `1..=2^20`; a source's out-of-range answer is taken mod `m`, so no bucket is ever out of range.
fn choice(choose: &mut impl FnMut(u64) -> u64, m: u64) -> u32 {
    u32::try_from(choose(m) % m).unwrap_or(0)
}

fn within(choose: &mut impl FnMut(u64) -> u64, (lo, hi): (u32, u32)) -> u32 {
    lo + choice(choose, u64::from(hi - lo) + 1)
}

/// One draw of `m6-pools v1` from `r`; every draw is in range, and a test checks it.
/// # Errors
/// The plan's refusal, were a draw ever out of range.
pub fn draw(r: &mut SplitMix) -> Result<SchedulePlan, PlanRefusal> {
    draw_with(&mut |m| r.below(m))
}

/// [`draw`] over any choice source: `choose(m)` is a choice in `0..m`, asked in `m6-pools v1`'s order, so the product of the `m` asked is [`one_in`] of the plan drawn.
/// # Errors
/// The plan's refusal, were a choice out of range.
pub(super) fn draw_with(choose: &mut impl FnMut(u64) -> u64) -> Result<SchedulePlan, PlanRefusal> {
    let split = match choose(SPLITS) {
        0 => Split::Shipped,
        _ => match choice(choose, GAP_BUCKETS) {
            0 => Split::SettleAtStep { gap_us: 0 },
            b => Split::SettleAtStep {
                gap_us: within(choose, gap_bucket(b)),
            },
        },
    };
    let wake = match choose(WAKES) {
        0 => Wake::NextStep,
        1 => Wake::ColdOnly,
        2 => {
            let c = FIRST_PERIOD_BUCKET + choice(choose, PERIOD_BUCKETS);
            Wake::Poll {
                period_us: within(choose, period_bucket(c)),
            }
        }
        3 => Wake::Cadence,
        _ => Wake::Never,
    };
    SchedulePlan::new(split, wake)
}

/// `1 / p`, where `p` is the idealised probability that one draw of `m6-pools v1` yields `plan`, each `below(m)` taken as exactly `1 / m` (its modulo bias is under `3e-14` relative).
#[must_use]
pub const fn one_in(plan: &SchedulePlan) -> u64 {
    const fn width((lo, hi): (u32, u32)) -> u64 {
        (hi - lo) as u64 + 1
    }
    let split = match plan.split() {
        Split::Shipped => SPLITS,
        Split::SettleAtStep { gap_us: 0 } => SPLITS * GAP_BUCKETS,
        Split::SettleAtStep { gap_us } => {
            SPLITS * GAP_BUCKETS * width(gap_bucket(32 - gap_us.leading_zeros()))
        }
    };
    let wake = match plan.wake() {
        Wake::Poll { period_us } => {
            WAKES * PERIOD_BUCKETS * width(period_bucket(31 - period_us.leading_zeros()))
        }
        _ => WAKES,
    };
    split * wake
}

/// Whether one draw yields `plan` with probability above `2^-MASS_CAP_LOG2`.
#[must_use]
pub const fn high_mass(plan: &SchedulePlan) -> bool {
    one_in(plan) < 1 << MASS_CAP_LOG2
}

const fn split_eq(a: Split, b: Split) -> bool {
    match (a, b) {
        (Split::Shipped, Split::Shipped) => true,
        (Split::SettleAtStep { gap_us: a }, Split::SettleAtStep { gap_us: b }) => a == b,
        _ => false,
    }
}

const fn wake_eq(a: Wake, b: Wake) -> bool {
    match (a, b) {
        (Wake::NextStep, Wake::NextStep)
        | (Wake::ColdOnly, Wake::ColdOnly)
        | (Wake::Cadence, Wake::Cadence)
        | (Wake::Never, Wake::Never) => true,
        (Wake::Poll { period_us: a }, Wake::Poll { period_us: b }) => a == b,
        _ => false,
    }
}

const fn in_table(plan: &SchedulePlan, table: &[(Split, Wake)]) -> bool {
    let mut i = 0;
    while i < table.len() {
        let (split, wake) = table[i];
        if split_eq(plan.split(), split) && wake_eq(plan.wake(), wake) {
            return true;
        }
        i += 1;
    }
    false
}

/// Why a pool was not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolRefusal {
    /// It did not fill within [`MAX_DRAWS`] draws.
    PoolExhausted {
        /// The pool.
        pool: PoolId,
    },
    /// A draw was out of range.
    Draw {
        /// The pool.
        pool: PoolId,
        /// Why.
        refusal: PlanRefusal,
    },
}

impl fmt::Display for PoolRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolRefusal::PoolExhausted { pool } => write!(
                f,
                "pool {} of set {:?} {} did not fill within {MAX_DRAWS} draws",
                pool.pool, pool.set.tag, pool.set.index
            ),
            PoolRefusal::Draw { pool, refusal } => write!(
                f,
                "a draw of pool {} of set {:?} {} is out of range: {refusal}",
                pool.pool, pool.set.tag, pool.set.index
            ),
        }
    }
}

impl std::error::Error for PoolRefusal {}

/// The pool's seed, binding the policy's name and version.
#[must_use]
pub fn pool_seed(pool: &PoolId) -> u64 {
    pool_seed_at(RANKER_POLICY_VERSION, pool)
}

fn pool_seed_at(version: u32, pool: &PoolId) -> u64 {
    let [i, p] = pool.parts();
    first_u64(&Digest::of(
        POOL_DOMAIN,
        &[
            RANKER_POLICY_NAME.as_bytes(),
            &version.to_le_bytes(),
            &[pool.set.tag.tag()],
            &i,
            &p,
        ],
    ))
}

/// The first [`POOL_PLANS`] distinct plans the pool's draws yield that `accepts` takes, in draw order.
/// # Errors
/// [`PoolRefusal::PoolExhausted`] past [`MAX_DRAWS`] draws.
pub fn fill(
    pool: &PoolId,
    accepts: impl Fn(&SchedulePlan) -> bool,
) -> Result<Vec<SchedulePlan>, PoolRefusal> {
    fill_at(RANKER_POLICY_VERSION, pool, accepts)
}

fn fill_at(
    version: u32,
    pool: &PoolId,
    accepts: impl Fn(&SchedulePlan) -> bool,
) -> Result<Vec<SchedulePlan>, PoolRefusal> {
    let mut r = SplitMix::new(pool_seed_at(version, pool));
    let mut out: Vec<SchedulePlan> = Vec::with_capacity(POOL_PLANS);
    for _ in 0..MAX_DRAWS {
        let plan = draw(&mut r).map_err(|refusal| PoolRefusal::Draw {
            pool: *pool,
            refusal,
        })?;
        if accepts(&plan) && !out.contains(&plan) {
            out.push(plan);
            if out.len() == POOL_PLANS {
                return Ok(out);
            }
        }
    }
    Err(PoolRefusal::PoolExhausted { pool: *pool })
}

/// Whether a held-out, report or practice set skips `plan`: a grid plan, a plans-file plan or a high-mass plan.
#[must_use]
pub const fn skipped(plan: &SchedulePlan) -> bool {
    in_table(plan, &GRID_V1) || in_table(plan, &PLANS_FILE_V1) || high_mass(plan)
}

/// The number of pools in `set`.
#[must_use]
pub const fn pools_in(set: SetId) -> usize {
    match set.tag {
        SetTag::Train => TRAIN_POOLS,
        _ => POOLS,
    }
}

/// Every pool of `set`, in pool order.
/// # Errors
/// The first pool that does not fill.
pub fn set_pools(set: SetId) -> Result<Vec<Vec<SchedulePlan>>, PoolRefusal> {
    set_pools_at(RANKER_POLICY_VERSION, set)
}

/// [`set_pools`] with its seeds bound to `version`.
pub(super) fn set_pools_at(
    version: u32,
    set: SetId,
) -> Result<Vec<Vec<SchedulePlan>>, PoolRefusal> {
    let pools = |set: SetId, also: &dyn Fn(&SchedulePlan) -> bool| {
        let count = pools_in(set);
        let mut out = Vec::with_capacity(count);
        for p in 0..count {
            let id = PoolId {
                set,
                pool: u32::try_from(p).unwrap_or(u32::MAX),
            };
            out.push(fill_at(version, &id, |plan| {
                side(plan) == set.tag.side()
                    && (set.tag == SetTag::Train || !skipped(plan))
                    && also(plan)
            })?);
        }
        Ok(out)
    };
    match set.tag {
        SetTag::PracticeReport => {
            let looks = pools(
                SetId {
                    tag: SetTag::PracticeHeldOut,
                    index: set.index,
                },
                &|_| true,
            )?;
            pools(set, &|plan| {
                for pool in &looks {
                    for p in pool {
                        if p == plan {
                            return false;
                        }
                    }
                }
                true
            })
        }
        _ => pools(set, &|_| true),
    }
}

/// The sets a v1 session can open: the train set, `H_1..H_3`, `E_1..E_3` and the rehearsal's pair.
#[must_use]
pub fn sets_v1() -> Vec<SetId> {
    let mut sets = vec![SetId {
        tag: SetTag::Train,
        index: 0,
    }];
    for i in 1..=3 {
        sets.push(SetId {
            tag: SetTag::HeldOut,
            index: i,
        });
        sets.push(SetId {
            tag: SetTag::Report,
            index: i,
        });
    }
    for tag in [SetTag::PracticeHeldOut, SetTag::PracticeReport] {
        sets.push(SetId { tag, index: 0 });
    }
    sets
}

/// The digest of every plan of every pool of [`sets_v1`], in order, which [`M6_SETS_V1_DIGEST`] pins.
/// # Errors
/// The first pool that does not fill.
pub fn sets_digest() -> Result<Digest, PoolRefusal> {
    let mut e = Enc::new();
    for set in sets_v1() {
        e.u8(set.tag.tag());
        e.u32(set.index);
        let pools = set_pools(set)?;
        e.u64(wide(pools.len()));
        for pool in pools {
            for plan in pool {
                e.raw(&plan.encode());
            }
        }
    }
    Ok(Digest::of(SETS_DOMAIN, &[&e.finish()]))
}

/// Why the M6 policy is not the pinned v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RankerRefusal {
    /// The policy's digest is not [`M6_POLICY_V1_DIGEST`].
    PolicyMutated {
        /// The digest found.
        found: Digest,
        /// The pinned digest.
        pinned: Digest,
    },
    /// The M4 policy the descriptor binds is not [`AcceptancePolicy::v1`]'s.
    AcceptanceMoved {
        /// The M4 digest bound.
        bound: Digest,
        /// [`AcceptancePolicy::v1`]'s digest now.
        found: Digest,
    },
    /// The sets drawn are not the pinned ones.
    SetsMoved {
        /// Their digest now.
        found: Digest,
    },
    /// A pool did not fill.
    Pool(PoolRefusal),
}

impl fmt::Display for RankerRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RankerRefusal::PolicyMutated { found, pinned } => write!(
                f,
                "model-admission policy refused: its digest {} is not the pinned {}; any change is a new policy version",
                found.short(),
                pinned.short()
            ),
            RankerRefusal::AcceptanceMoved { bound, found } => write!(
                f,
                "model-admission policy refused: it binds M4 policy {}, but M4's v1 is now {}; a re-pin of M4 is a new M6 policy version",
                bound.short(),
                found.short()
            ),
            RankerRefusal::SetsMoved { found } => write!(
                f,
                "model-admission policy refused: the drawn sets digest to {}, not the pinned {}",
                found.short(),
                M6_SETS_V1_DIGEST.short()
            ),
            RankerRefusal::Pool(e) => write!(f, "model-admission policy refused: {e}"),
        }
    }
}

impl std::error::Error for RankerRefusal {}

/// What the policy binds, in descriptor order.
#[derive(Debug, PartialEq, Eq)]
struct Terms {
    acceptance: Digest,
    features: usize,
    pool_plans: usize,
    judged: usize,
    pools: usize,
    train_pools: usize,
    explore: usize,
    epochs: u32,
    rounds: u8,
    max_steps: u64,
    max_draws: u32,
    mass_cap_log2: u32,
    max_labels: usize,
    max_judged: usize,
    looks: u8,
    look_level: u128,
    report_level: u128,
    look_list_cap: usize,
    read_list_cap: usize,
    store: [u64; 6],
    deadline_s: u64,
    footprint: u64,
    plans_file: Vec<SchedulePlan>,
    base: Digest,
    rules: [&'static str; 14],
    domains: [&'static [u8]; 9],
}

#[inline]
const fn wide(n: usize) -> u64 {
    n as u64
}

impl Terms {
    fn v1() -> Terms {
        let mut plans_file = Vec::with_capacity(PLANS_FILE_V1.len());
        for &(split, wake) in &PLANS_FILE_V1 {
            if let Ok(plan) = SchedulePlan::new(split, wake) {
                plans_file.push(plan);
            }
        }
        Terms {
            acceptance: M4_POLICY_V1_DIGEST,
            features: FEATURES,
            pool_plans: POOL_PLANS,
            judged: JUDGED,
            pools: POOLS,
            train_pools: TRAIN_POOLS,
            explore: EXPLORE,
            epochs: EPOCHS,
            rounds: ROUNDS,
            max_steps: MAX_STEPS,
            max_draws: MAX_DRAWS,
            mass_cap_log2: MASS_CAP_LOG2,
            max_labels: MAX_LABELS,
            max_judged: MAX_JUDGED,
            looks: LOOKS,
            look_level: LOOK_LEVEL,
            report_level: REPORT_LEVEL,
            look_list_cap: LOOK_LIST_CAP,
            read_list_cap: READ_LIST_CAP,
            store: [
                wide(MAX_VERSIONS),
                MAX_VERSION_BYTES,
                wide(MAX_REVISIONS),
                MAX_LINE_BYTES,
                MAX_FIGURES_BYTES,
                MAX_REPORT_BYTES,
            ],
            deadline_s: CHILD_DEADLINE_S,
            footprint: MAX_FOOTPRINT,
            plans_file,
            base: base_digest(),
            rules: [
                FEATURES_RULE,
                RANK_RULE,
                TRAIN_RULE,
                DATA_RULE,
                CONTROL_RULE,
                POOLS_RULE,
                SIDE_RULE,
                SKIP_RULE,
                ADMISSION_RULE,
                LOOK_ORDER_RULE,
                LEAKAGE_RULE,
                W1_RULE,
                W2_RULE,
                ENCODING_RULE,
            ],
            domains: [
                POOL_DOMAIN,
                SIDE_DOMAIN,
                TIE_DOMAIN,
                CONTROL_DOMAIN,
                CONTROL_ID_DOMAIN,
                TRAINER_DOMAIN,
                VERSION_DOMAIN,
                BASE_DOMAIN,
                SETS_DOMAIN,
            ],
        }
    }

    fn descriptor(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.digest(&self.acceptance);
        for n in [
            self.features,
            self.pool_plans,
            self.judged,
            self.pools,
            self.train_pools,
            self.explore,
        ] {
            e.u64(wide(n));
        }
        e.u32(self.epochs);
        e.u8(self.rounds);
        e.u64(self.max_steps);
        e.u32(self.max_draws);
        e.u32(self.mass_cap_log2);
        e.u64(wide(self.max_labels));
        e.u64(wide(self.max_judged));
        e.u8(self.looks);
        e.raw(&self.look_level.to_le_bytes());
        e.raw(&self.report_level.to_le_bytes());
        e.u64(wide(self.look_list_cap));
        e.u64(wide(self.read_list_cap));
        for n in self.store {
            e.u64(n);
        }
        e.u64(self.deadline_s);
        e.u64(self.footprint);
        e.u64(wide(self.plans_file.len()));
        for p in &self.plans_file {
            e.raw(&p.encode());
        }
        e.digest(&self.base);
        for rule in self.rules {
            e.str(rule);
        }
        for domain in self.domains {
            e.bytes(domain);
        }
        e.finish()
    }
}

/// The model-admission policy `momentum-glow.m6-ranker-admission` v1.
#[derive(Debug, PartialEq, Eq)]
pub struct RankerPolicy {
    id: ArtifactId,
    terms: Terms,
}

impl RankerPolicy {
    /// The policy v1 from this module's constants, unchecked: a session uses [`Self::pinned`].
    #[must_use]
    pub fn v1() -> RankerPolicy {
        Self::of(Terms::v1())
    }

    fn of(terms: Terms) -> RankerPolicy {
        let id = ArtifactId::new(
            ArtifactKind::Policy,
            RANKER_POLICY_NAME,
            RANKER_POLICY_VERSION,
            &terms.descriptor(),
        );
        RankerPolicy { id, terms }
    }

    /// [`Self::v1`], returned only when its digest is the pin, the M4 policy it binds is M4's v1 now, and the sets it draws are the pinned ones.
    /// # Errors
    /// [`RankerRefusal::PolicyMutated`], then [`RankerRefusal::AcceptanceMoved`], then a pool's refusal or [`RankerRefusal::SetsMoved`].
    pub fn pinned() -> Result<RankerPolicy, RankerRefusal> {
        Self::pinned_from(Self::v1(), AcceptancePolicy::v1().id().digest, sets_digest)
    }

    /// `policy` against the pin, then against `m4`, M4's v1 digest now, then the sets `sets` draws against theirs.
    fn pinned_from(
        policy: RankerPolicy,
        m4: Digest,
        sets: impl FnOnce() -> Result<Digest, PoolRefusal>,
    ) -> Result<RankerPolicy, RankerRefusal> {
        policy.check_pin()?;
        policy.check_acceptance(m4)?;
        let found = sets().map_err(RankerRefusal::Pool)?;
        if found != M6_SETS_V1_DIGEST {
            return Err(RankerRefusal::SetsMoved { found });
        }
        Ok(policy)
    }

    /// Its artifact.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// The M4 acceptance policy's digest it binds, which a store records.
    #[inline]
    #[must_use]
    pub const fn acceptance(&self) -> Digest {
        self.terms.acceptance
    }

    fn check_pin(&self) -> Result<(), RankerRefusal> {
        if self.id.digest == M6_POLICY_V1_DIGEST {
            Ok(())
        } else {
            Err(RankerRefusal::PolicyMutated {
                found: self.id.digest,
                pinned: M6_POLICY_V1_DIGEST,
            })
        }
    }

    fn check_acceptance(&self, found: Digest) -> Result<(), RankerRefusal> {
        if self.terms.acceptance == found {
            Ok(())
        } else {
            Err(RankerRefusal::AcceptanceMoved {
                bound: self.terms.acceptance,
                found,
            })
        }
    }
}

/// A term of the descriptor as the test-only mutation hook alters it, in descriptor order.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Acceptance,
    Features,
    PoolPlans,
    Judged,
    Pools,
    TrainPools,
    Explore,
    Epochs,
    Rounds,
    MaxSteps,
    MaxDraws,
    MassCap,
    MaxLabels,
    MaxJudged,
    Looks,
    LookLevel,
    ReportLevel,
    LookListCap,
    ReadListCap,
    Store(usize),
    Deadline,
    Footprint,
    PlansFileDropped,
    PlansFileOrder,
    PlansFilePlan,
    Base,
    Rule(usize),
    Domain(usize),
}

#[cfg(test)]
impl Field {
    /// Every field, in descriptor order.
    pub(super) fn all() -> Vec<Field> {
        // Each term is named here, so a new one fails to compile until it has a `Field`; a
        // `Field` left out of the list below is never constructed, which `-D warnings` refuses.
        let Terms {
            acceptance: _,
            features: _,
            pool_plans: _,
            judged: _,
            pools: _,
            train_pools: _,
            explore: _,
            epochs: _,
            rounds: _,
            max_steps: _,
            max_draws: _,
            mass_cap_log2: _,
            max_labels: _,
            max_judged: _,
            looks: _,
            look_level: _,
            report_level: _,
            look_list_cap: _,
            read_list_cap: _,
            store: _,
            deadline_s: _,
            footprint: _,
            plans_file: _,
            base: _,
            rules: _,
            domains: _,
        } = Terms::v1();
        let mut all = vec![
            Field::Acceptance,
            Field::Features,
            Field::PoolPlans,
            Field::Judged,
            Field::Pools,
            Field::TrainPools,
            Field::Explore,
            Field::Epochs,
            Field::Rounds,
            Field::MaxSteps,
            Field::MaxDraws,
            Field::MassCap,
            Field::MaxLabels,
            Field::MaxJudged,
            Field::Looks,
            Field::LookLevel,
            Field::ReportLevel,
            Field::LookListCap,
            Field::ReadListCap,
        ];
        all.extend((0..6).map(Field::Store));
        all.extend([
            Field::Deadline,
            Field::Footprint,
            Field::PlansFileDropped,
            Field::PlansFileOrder,
            Field::PlansFilePlan,
            Field::Base,
        ]);
        all.extend((0..14).map(Field::Rule));
        all.extend((0..9).map(Field::Domain));
        all
    }
}

#[cfg(test)]
impl Terms {
    fn mutate(&mut self, field: Field) {
        match field {
            Field::Acceptance => self.acceptance.0[0] ^= 1,
            Field::Features => self.features += 1,
            Field::PoolPlans => self.pool_plans += 1,
            Field::Judged => self.judged += 1,
            Field::Pools => self.pools += 1,
            Field::TrainPools => self.train_pools += 1,
            Field::Explore => self.explore += 1,
            Field::Epochs => self.epochs += 1,
            Field::Rounds => self.rounds += 1,
            Field::MaxSteps => self.max_steps += 1,
            Field::MaxDraws => self.max_draws += 1,
            Field::MassCap => self.mass_cap_log2 += 1,
            Field::MaxLabels => self.max_labels += 1,
            Field::MaxJudged => self.max_judged += 1,
            Field::Looks => self.looks += 1,
            Field::LookLevel => self.look_level += 1,
            Field::ReportLevel => self.report_level += 1,
            Field::LookListCap => self.look_list_cap += 1,
            Field::ReadListCap => self.read_list_cap += 1,
            Field::Store(k) => self.store[k] += 1,
            Field::Deadline => self.deadline_s += 1,
            Field::Footprint => self.footprint += 1,
            Field::PlansFileDropped => {
                self.plans_file.pop();
            }
            Field::PlansFileOrder => self.plans_file.swap(0, 1),
            Field::PlansFilePlan => {
                self.plans_file[0] =
                    SchedulePlan::new(Split::SettleAtStep { gap_us: 4_001 }, Wake::NextStep)
                        .expect("a plan in range");
            }
            Field::Base => self.base.0[0] ^= 1,
            Field::Rule(k) => self.rules[k] = "a changed rule",
            Field::Domain(k) => self.domains[k] = b"a-changed-domain\0",
        }
    }
}

#[cfg(test)]
impl RankerPolicy {
    /// v1 with `field` changed.
    pub(super) fn mutated(field: Field) -> RankerPolicy {
        let mut terms = Terms::v1();
        terms.mutate(field);
        Self::of(terms)
    }

    /// The pin check alone.
    pub(super) fn pin(&self) -> Result<(), RankerRefusal> {
        self.check_pin()
    }

    /// [`Self::pinned`]'s checks of `policy`, against `m4` and the sets `sets` draws.
    pub(super) fn pinned_with(
        policy: RankerPolicy,
        m4: Digest,
        sets: impl FnOnce() -> Result<Digest, PoolRefusal>,
    ) -> Result<RankerPolicy, RankerRefusal> {
        Self::pinned_from(policy, m4, sets)
    }
}
