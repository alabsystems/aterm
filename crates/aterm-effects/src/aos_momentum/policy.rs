// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `momentum-glow.m4-acceptance` v1.

use core::fmt;
use std::time::Duration;

use aterm_aos::contract::Contract;
use aterm_aos::effects::NoEffects;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::replay::replay;
use aterm_aos::task::{Code, Schema, state_digest};
use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, ArtifactKind, Digest};
use aterm_time::Instant;

use super::corpus::{
    Corpus, CorpusSplit, GENERATOR_VERSION, GOLDEN_TRACE_DOMAIN, Item, ItemBody, ItemKind, Stratum,
    corpus_digest, item_part, scenario,
};
use super::fidelity::{LaneFidelity, PROBE_DELTA};
use super::lane::{LANE_MODEL, SCENARIO_FORMAT, Scenario, encode_glass, run, s0};
use super::plan::{
    MAX_GAP_US, MAX_PERIOD_US, MIN_PERIOD_US, PLAN_ARTIFACT_NAME, PLAN_DOMAIN, PLAN_FORMAT,
    PLAN_INTERPRETER, PLAN_LEN, SchedulePlan, Split, Wake,
};
use super::{
    DEMO_CODE_NAME, HostMode, MOMENTUM_MAX_EVENTS, MOMENTUM_QUEUE_CAP, MomentumCode, MomentumHost,
    MomentumObservation, MomentumTrace, NoStrandedLight, PrebuiltMomentum, SHIPPED_CODE_NAME,
    Sched, profile_artifact, schema_artifact,
};
use crate::cursor_momentum::MomentumGlowFrame;

/// The policy artifact's name.
pub const POLICY_NAME: &str = "momentum-glow.m4-acceptance";

/// The policy artifact's version.
pub const POLICY_VERSION: u32 = 1;

/// The digest of [`AcceptancePolicy::v1`]'s artifact, pinned, which only a reviewed source commit
/// moves.
pub const M4_POLICY_V1_DIGEST: Digest =
    hex_digest("eec55f48cc48f699695e580cd921ececc9d2a8e94292f65174e7fb0e7ed9ab37");

/// The corpus digest generator v1 makes ([`Corpus::digest`]), pinned.
pub const CORPUS_V1_DIGEST: Digest =
    hex_digest("5ff4befccd385e929592626d775ad6d8a9df530cf0546991d468b2f092cbc2a5");

/// The training items: the golden trace and the 16 generated training seeds.
pub const TRAIN_COUNT: usize = 17;

/// The held-out items: `S0` and the 8 generated held-out seeds.
pub const HELD_OUT_COUNT: usize = 9;

/// The most rounds a search asks its proposer for.
pub const MAX_ROUNDS: u8 = 4;

/// The most distinct candidates (by plan digest, under [`PLAN_DOMAIN`]) a search evaluates.
pub const MAX_DISTINCT: usize = 256;

/// A proposal longer than this many bytes is refused before it is decoded.
pub const MAX_PROPOSAL_BYTES: usize = 64;

/// The splits of the grid, in order.
pub const GRID_SPLITS: [Split; 9] = [
    Split::Shipped,
    Split::SettleAtStep { gap_us: 0 },
    Split::SettleAtStep { gap_us: 8_333 },
    Split::SettleAtStep { gap_us: 16_667 },
    Split::SettleAtStep { gap_us: 25_000 },
    Split::SettleAtStep { gap_us: 33_334 },
    Split::SettleAtStep { gap_us: 50_000 },
    Split::SettleAtStep { gap_us: 100_000 },
    Split::SettleAtStep { gap_us: 250_000 },
];

/// The wakes of the grid, in order.
pub const GRID_WAKES: [Wake; 6] = [
    Wake::NextStep,
    Wake::ColdOnly,
    Wake::Poll { period_us: 16_667 },
    Wake::Poll { period_us: 33_334 },
    Wake::Cadence,
    Wake::Never,
];

/// The grid v1.
pub const GRID_V1: [(Split, Wake); 54] = grid_v1();

/// The rule that picks the training winner.
pub const SELECTION_RULE: &str = "m4-selection v1: among the candidates admitted on every \
     training item, the lowest training sum T, then the lowest training sum Z, then the lowest \
     plan bytes; the winner's training sum T and sum Z must both be below the baseline's, or \
     there is no outcome";

/// The rule that reads the held-out split.
pub const HELD_OUT_RULE: &str = "m4-held-out v1: the held-out split is read once, for the \
     training winner only; a refusal there is no outcome, and no runner-up is tried";

/// The rule that makes an admitted plan useful.
pub const USEFUL_RULE: &str = "m4-useful v1: Useful = admitted on every training item and \
     every held-out item, held-out sum T_c < sum T_B and held-out sum Z_c < sum Z_B; \
     gain = 1 - sum T_c / sum T_B over the held-out split, reported for S0 alone too, with Z";

/// The rule that admits a plan on the corpus.
pub const ADMISSION_RULE: &str = "m4-admission v1: a proposal over the byte budget is refused \
     before decode; the plan decodes; it enters on the golden trace's checkpoint; the golden \
     trace's log replayed from its checkpoint with the plan passes NoStrandedLight; on every lane \
     item a host attached with the plan and driven by the lane reaches the scenario's end, logs \
     every call, its log replayed from its checkpoint reproduces the live head and passes \
     NoStrandedLight, and the run passes lane fidelity; a refusal on any item refuses the \
     candidate, and no item leaves the denominator";

/// The grade a result under this policy can carry, at most.
pub const GRADE_CAP: EvidenceGrade = EvidenceGrade::Tested;

const BASELINE_BUILD: &str = "aterm-aos-m4-baseline";

#[inline]
const fn grid_v1() -> [(Split, Wake); 54] {
    let mut g = [(Split::Shipped, Wake::NextStep); 54];
    let mut i = 0;
    while i < 54 {
        g[i] = (GRID_SPLITS[i / 6], GRID_WAKES[i % 6]);
        i += 1;
    }
    g
}

#[inline]
const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("a digest pin is lowercase hex"),
    }
}

#[inline]
#[must_use]
pub(super) const fn hex_digest(hex: &str) -> Digest {
    let b = hex.as_bytes();
    assert!(b.len() == 64, "a digest pin is 64 hex digits");
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    Digest(out)
}

#[inline]
const fn grade_name(g: EvidenceGrade) -> &'static str {
    match g {
        EvidenceGrade::Tested => "Tested",
    }
}

/// What the shipped baseline does on one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Baseline {
    /// `T`: ticks rendered.
    pub ticks: u64,
    /// `P`: ticks whose frame differs from the frame already on glass.
    pub presented: u64,
    /// `Z = T − P`.
    pub wasted: u64,
    /// The lane's FNV-1a digest ([`LaneRun::fnv`](super::lane::LaneRun::fnv)).
    pub lane: Option<u64>,
    /// The observation chain of the replay `NoStrandedLight` judges.
    pub chain: Digest,
}

/// One item of the policy's corpus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemPin {
    id: &'static str,
    kind: ItemKind,
    split: CorpusSplit,
    content: Digest,
    baseline: Baseline,
}

impl ItemPin {
    /// Its stable id.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    /// Its kind.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> ItemKind {
        self.kind
    }

    /// Its split.
    #[inline]
    #[must_use]
    pub const fn split(&self) -> CorpusSplit {
        self.split
    }

    /// The digest of what it runs.
    #[inline]
    #[must_use]
    pub const fn content(&self) -> Digest {
        self.content
    }

    /// The shipped baseline on it.
    #[inline]
    #[must_use]
    pub const fn baseline(&self) -> Baseline {
        self.baseline
    }
}

#[inline]
const fn pin(
    id: &'static str,
    kind: ItemKind,
    split: CorpusSplit,
    content: &str,
    tpz: (u64, u64, u64),
    lane: Option<u64>,
    chain: &str,
) -> ItemPin {
    ItemPin {
        id,
        kind,
        split,
        content: hex_digest(content),
        baseline: Baseline {
            ticks: tpz.0,
            presented: tpz.1,
            wasted: tpz.2,
            lane,
            chain: hex_digest(chain),
        },
    }
}

#[inline]
const fn generated(seed: u64, stratum: Stratum) -> ItemKind {
    ItemKind::Generated { seed, stratum }
}

/// The corpus and the shipped baseline.
pub const ITEMS_V1: &[ItemPin] = &[
    pin(
        "golden",
        ItemKind::GoldenTrace,
        CorpusSplit::Train,
        "a2b1a1140c0579afe449c2a5d900ea9dd495cb318de5b535f16aef1932549a2e",
        (154, 130, 24),
        None,
        "90a74ca21311491a8deaaeb9bac9e17bf1af694dbeeff9258a395d32d8170f71",
    ),
    pin(
        "train-01",
        generated(1, Stratum::CaretAtKeys),
        CorpusSplit::Train,
        "cf410ba5b8ac1e48790cfc4c48ff1288f8bd9bea8461e86406c5f454e789616e",
        (122, 102, 20),
        Some(0x43af_1cf3_be18_7e20),
        "3ee8b63641e52db58b9f3ad027a1e04f1f9a73dad754f3b8518a5cec4b21c0b8",
    ),
    pin(
        "train-02",
        generated(2, Stratum::ConfigAtKeys),
        CorpusSplit::Train,
        "af4648038873c61315604adf7cf760e5ea7647b12ee688aabf42d5accf3b3029",
        (1_083, 751, 332),
        Some(0x250c_5c8c_0075_e552),
        "bba2f2d86eb04e747f7a858d07a5200652a5310348957a243c65ea99e102fddc",
    ),
    pin(
        "train-03",
        generated(3, Stratum::MovingHue),
        CorpusSplit::Train,
        "5e35f8cbf26e947a7d6eea66f7d98fa77360917d77c1e658d1fd2668737e7454",
        (1_484, 1_147, 337),
        Some(0xb150_2145_4217_1186),
        "ac042b3804b651c7e90f7fbe04fc22b85f69c585f21170948fa68fe4bb959d2b",
    ),
    pin(
        "train-04",
        generated(4, Stratum::IntensityRamp),
        CorpusSplit::Train,
        "548774c062d0c290c1360f0b07f0fbf6184e03ca9a7f34277728795d8d742523",
        (319, 274, 45),
        Some(0xe78f_1899_d5c1_92ea),
        "d5a3b15737569687185dcfe089b47f3f293bdca9c7575125e60d6cb2f2f15f23",
    ),
    pin(
        "train-05",
        generated(5, Stratum::ResetInCoolDown),
        CorpusSplit::Train,
        "c6d67dc27c1f989fc65325b1c951db5e642fa92802ac03fd7dca1a1a2750498d",
        (105, 100, 5),
        Some(0xf5bd_43c5_028f_d049),
        "67904123d49732a992c4007f05c19b14a17e60c2dfd28b0c5536c7f968e66733",
    ),
    pin(
        "train-06",
        generated(6, Stratum::Plain),
        CorpusSplit::Train,
        "fcff544899c33686ce08e9346c10f13bfe3e1df67543c3f7d72b6611657aca79",
        (283, 193, 90),
        Some(0xb0ff_1665_889e_d567),
        "db2ae4d60ef1fee2f57fcdca4ff9dd33e9d7e4b6b93ba1710fc10e50a731ccb5",
    ),
    pin(
        "train-07",
        generated(7, Stratum::CaretAtKeys),
        CorpusSplit::Train,
        "284df17943d0732383f190dd5ef2b677576441d4c181aaac2e99363e1aa20e86",
        (483, 429, 54),
        Some(0xb974_405c_86b5_a993),
        "77d98589637da6014d4c6a4b4d2ae2ec4afe8f9a7f7bac4315461c03fb80b7f4",
    ),
    pin(
        "train-08",
        generated(8, Stratum::ConfigAtKeys),
        CorpusSplit::Train,
        "a5b4815407a0bbdd1b0a21334235fe6bffcf320d6d8f074b25128d54ca081705",
        (530, 269, 261),
        Some(0x9a99_ae9a_73cb_1851),
        "a96a9fbd4f4a1c4ed6796405e0b5d3f70d68e956fd64b836b3c1a11b4186bcbe",
    ),
    pin(
        "train-09",
        generated(9, Stratum::MovingHue),
        CorpusSplit::Train,
        "29c1425e82d111ca792570382f04c0ef6b9f236a12713900dad7424655f207df",
        (498, 447, 51),
        Some(0xb200_41b8_f493_d99f),
        "a3c69606c6241608df24590e303b1cd99f9674b235562b8f92fabc3bbadaf09d",
    ),
    pin(
        "train-10",
        generated(10, Stratum::IntensityRamp),
        CorpusSplit::Train,
        "35c84a26cca125d28d149702f12dcaa5f0c508bb84cc5b03810e55b15f37fe6c",
        (1_144, 689, 455),
        Some(0x5da9_13ef_8721_89ac),
        "52761d2c6b527584a5e748b0fad23abffb8422150afd2a028679e75ce4a777c2",
    ),
    pin(
        "train-11",
        generated(11, Stratum::ResetInCoolDown),
        CorpusSplit::Train,
        "56ed94930d01095e447866d2678936ccbae7c672e5054da6a3d6dfd6a23d8b8e",
        (1_426, 1_053, 373),
        Some(0x7027_80fe_df07_4490),
        "cb66265471194a938a174ff7891620b1c17961a8b8e7b247e7c2e05d4827a7c4",
    ),
    pin(
        "train-12",
        generated(12, Stratum::Plain),
        CorpusSplit::Train,
        "7eb40f37cd8dbb3f23a47783935a0400d4a7b34d4c852ede80533234c73f2dee",
        (153, 81, 72),
        Some(0x84b0_8c97_0873_86fc),
        "8fcde5ecf1523d08d19aeb9bfdd8a571968804c3de70d87b67efb8c269e4bdd6",
    ),
    pin(
        "train-13",
        generated(13, Stratum::CaretAtKeys),
        CorpusSplit::Train,
        "91243e1f490540f67eca7e5692b56e5d80ae39f7c828e760b5f5a7eeb318f78a",
        (140, 132, 8),
        Some(0xf6fd_c507_aaf1_72f1),
        "a0c1e09441ee106a234763f92e9783eb64dd9c07b2ecb078380d82d714890eb3",
    ),
    pin(
        "train-14",
        generated(14, Stratum::ConfigAtKeys),
        CorpusSplit::Train,
        "95b31811aa8114f9e6ed347db6069da2a2f09cfc775f7fd7f7c6d2d5e794b836",
        (263, 262, 1),
        Some(0x05a2_709b_2158_585d),
        "32df179f225411eaa7203c072c0222ddf7fff515af9148f58d28d7da1f9a6b4d",
    ),
    pin(
        "train-15",
        generated(15, Stratum::MovingHue),
        CorpusSplit::Train,
        "9e7f327d8c74aa527ab8bbea457081b2fa2aa5b63fab1e61499ec2815fc67530",
        (125, 103, 22),
        Some(0x4591_9036_ba03_6b94),
        "d4e76245d8d3bbcfcc3888442ac15036f63bf72761802836e6f93b8dd9b8f228",
    ),
    pin(
        "train-16",
        generated(16, Stratum::IntensityRamp),
        CorpusSplit::Train,
        "d90fc446e965a9f9847430c30bd58e78475abb82f1d733daf6f2246960c28965",
        (129, 104, 25),
        Some(0x6c25_fba3_8fe9_0057),
        "baa658c51f8d8d7b620ebb2245d84a0cd0c494c21221be08c6821352f77dde2e",
    ),
    pin(
        "s0",
        ItemKind::M3Story,
        CorpusSplit::HeldOut,
        "ff55bed06b0037421104e9a458d01d62b9031bb9493f2485466f3650f2d294b6",
        (2_776, 2_480, 296),
        Some(0xbf64_2e76_44df_7e95),
        "4027b9383f0bf7d444a92b412de9669f6d471dee72cd406b8dc7c5b5e0065a73",
    ),
    pin(
        "held-101",
        generated(101, Stratum::ResetInCoolDown),
        CorpusSplit::HeldOut,
        "5bfbe42b7dace142910dcf2a10a9a4f83b2c0b0ea42805f0124ad6263d8c8184",
        (1_180, 592, 588),
        Some(0x2568_f54a_6d22_e8ba),
        "9c915ba3a9e06548830ad72167dd0706bd2345402f19d55b95fc3fd0102f3ef0",
    ),
    pin(
        "held-102",
        generated(102, Stratum::Plain),
        CorpusSplit::HeldOut,
        "f17fdf1d52b316ca4b435be5adee08e2c2a54642332e6da2eb625416d4977deb",
        (620, 314, 306),
        Some(0x011b_fd8a_d1bc_e7e9),
        "791c828475b6ab47a68c82641af6b4ac620034a1211b95a4aa80397635bcab51",
    ),
    pin(
        "held-103",
        generated(103, Stratum::CaretAtKeys),
        CorpusSplit::HeldOut,
        "28df78ad3972559a9bcd5d971b67287f8e6af22f355781a264392e5372298a43",
        (361, 237, 124),
        Some(0x1320_5473_d170_800d),
        "4402260f7b46ffe285025fa8be5e31aea150c549f5406e77d2e9720b9f8a080d",
    ),
    pin(
        "held-104",
        generated(104, Stratum::ConfigAtKeys),
        CorpusSplit::HeldOut,
        "413032f151e853d25bcaa6698eb4ef2a381ad1d3837762b4d304d159302dddb2",
        (554, 369, 185),
        Some(0xda9b_1c9f_650d_155d),
        "adbe762810e4522849a2a5bf37d87573fa6374624e13b7d274d88edf64185cfc",
    ),
    pin(
        "held-105",
        generated(105, Stratum::MovingHue),
        CorpusSplit::HeldOut,
        "da5eeec58961017f003db2de950b0e357dcd88a245b76838eae25890688457fc",
        (313, 312, 1),
        Some(0x9aba_53ec_5205_8f9a),
        "508bf1c622bc91b62c58d89b7ddfb69c1648815933e450614ec245eb076524f5",
    ),
    pin(
        "held-106",
        generated(106, Stratum::IntensityRamp),
        CorpusSplit::HeldOut,
        "9ed21b1cbef617dd12e0470ea626b269038444b6b16f5bc086807b078fa837fe",
        (592, 371, 221),
        Some(0x0d56_2fa6_aeda_905f),
        "2592c9b77506064670470eb48dd21f62deac19c5bfe5a80fa8d6c6d10c4c44b8",
    ),
    pin(
        "held-107",
        generated(107, Stratum::ResetInCoolDown),
        CorpusSplit::HeldOut,
        "a2fac91426c52c8d6cbb4172fa3c0e9e44b9992da39090f5869a89bd89e83e05",
        (57, 48, 9),
        Some(0x196a_a45b_92e5_c4aa),
        "5e909c85009b559ac99aa0ebdce247e6a5b744ed5f1351233282c6934d4d3c91",
    ),
    pin(
        "held-108",
        generated(108, Stratum::Plain),
        CorpusSplit::HeldOut,
        "51cf2986d30b8e0cfd4145f71d773d5b7aa3e6ec3d73f6da3570bfa3a4c72164",
        (232, 212, 20),
        Some(0xcb99_f462_8d8b_5c5e),
        "a3393c1d573d01a90ead33fe68120eae592e9f1e0731609dfab963468988f201",
    ),
];

/// How a plan may replace a base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Replace {
    /// Replacing the shipped code is an optimization: the plan must be Useful.
    Optimization,
    /// Replacing the demonstration fault is a repair: the plan must be admitted.
    Repair,
}

impl Replace {
    /// Its tag in the descriptor.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Replace::Optimization => 0,
            Replace::Repair => 1,
        }
    }

    /// Its rule, as the descriptor binds it.
    #[inline]
    #[must_use]
    pub const fn rule(self) -> &'static str {
        match self {
            Replace::Optimization => "optimization v1: the plan must be Useful",
            Replace::Repair => "repair v1: the plan must be admitted",
        }
    }
}

/// The codes a plan may replace.
pub const BASES_V1: [(&str, Replace); 2] = [
    (SHIPPED_CODE_NAME, Replace::Optimization),
    (DEMO_CODE_NAME, Replace::Repair),
];

/// The budgets a search runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// The most rounds.
    pub rounds: u8,
    /// The most distinct candidates, by plan digest.
    pub distinct: usize,
    /// The longest proposal decoded, in bytes.
    pub proposal_bytes: usize,
}

/// `T`, `P` and `Z` summed over a split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// `ΣT`.
    pub ticks: u64,
    /// `ΣP`.
    pub presented: u64,
    /// `ΣZ`.
    pub wasted: u64,
}

/// What a drift names: the rows, a lost admission, or a value of the baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BaselineField {
    /// The measured rows are not the policy's items: one is missing, extra or another item's.
    Row,
    /// `T`.
    Ticks,
    /// `P`.
    Presented,
    /// `Z`.
    Wasted,
    /// The lane digest.
    LaneDigest,
    /// The observation chain.
    Chain,
    /// The shipped code is no longer admitted on a lane item.
    Admitted,
    /// The shipped code is no longer admitted on the golden trace.
    GoldenHead,
}

impl BaselineField {
    /// Its name in a refusal of a moved value and in a test's message.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BaselineField::Row => "row",
            BaselineField::Ticks => "T",
            BaselineField::Presented => "P",
            BaselineField::Wasted => "Z",
            BaselineField::LaneDigest => "lane digest",
            BaselineField::Chain => "chain",
            BaselineField::Admitted => "admitted",
            BaselineField::GoldenHead => "golden head",
        }
    }

    /// Whether a v1 re-pin may take a move in it.
    #[inline]
    #[must_use]
    pub const fn repinnable(self) -> bool {
        matches!(self, BaselineField::LaneDigest | BaselineField::Chain)
    }
}

/// The id a corpus drift names when every item matches its pin and the corpus digest does not.
pub const CORPUS_FOLD: &str = "corpus";

/// Why the policy is not the pinned v1, or its corpus or baseline moved, so no policy is built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyRefusal {
    /// The policy's digest is not [`M4_POLICY_V1_DIGEST`].
    PolicyMutated {
        /// The digest found.
        found: Digest,
        /// The pinned digest.
        pinned: Digest,
    },
    /// The corpus is not the policy's.
    CorpusDrift {
        /// The item's id.
        item: String,
    },
    /// The shipped code's run on `item` is not its pin or is not admitted.
    BaselineDrift {
        /// The item's id.
        item: String,
        /// What differs.
        field: BaselineField,
    },
}

impl fmt::Display for PolicyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyRefusal::PolicyMutated { found, pinned } => write!(
                f,
                "acceptance policy refused: its digest {} is not the pinned {}; re-pinning is a reviewed source commit",
                found.short(),
                pinned.short()
            ),
            PolicyRefusal::CorpusDrift { item } => {
                write!(
                    f,
                    "acceptance policy refused: the corpus moved at {item}, which v1 binds: fix the change, or make a new policy version"
                )
            }
            PolicyRefusal::BaselineDrift {
                item,
                field: BaselineField::Admitted,
            } => write!(
                f,
                "acceptance policy refused: the shipped code is not admitted at {item}, which no pin can take: fix what broke its admission (`the_baseline_is_pinned` reports every moved row)"
            ),
            PolicyRefusal::BaselineDrift {
                item,
                field: BaselineField::GoldenHead,
            } => write!(
                f,
                "acceptance policy refused: the shipped code is not admitted at {item}, the golden trace, and no v1 re-pin can take that: a change meant to move its recorded head needs a new trace and a new policy version; otherwise fix what broke its admission (`the_baseline_is_pinned` reports every moved row)"
            ),
            PolicyRefusal::BaselineDrift {
                item,
                field: BaselineField::Row,
            } => write!(
                f,
                "acceptance policy refused: the measured rows are not the policy's items at {item}; that is a new policy version (`the_baseline_is_pinned` reports every moved row)"
            ),
            PolicyRefusal::BaselineDrift { item, field } => write!(
                f,
                "acceptance policy refused: the shipped baseline moved at {item} ({}); {} (`the_baseline_is_pinned` reports every moved row)",
                field.name(),
                if field.repinnable() {
                    "no row moved anything but its lane digest or chain, so a v1 re-pin in a reviewed source commit may take the change"
                } else {
                    "that is a new policy version"
                }
            ),
        }
    }
}

impl std::error::Error for PolicyRefusal {}

/// What an item runs, as the checks read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Body<'a> {
    /// A trace file's bytes, replayed open loop.
    Trace(&'a [u8]),
    /// A scenario, run closed loop.
    Lane(&'a Scenario),
}

/// One corpus item as the corpus check and the baseline read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ItemFacts<'a> {
    /// The item's name, compared with its pin's.
    pub(super) id: &'a str,
    /// Whether it replays a trace or runs a lane.
    pub(super) kind: ItemKind,
    /// Train or held out.
    pub(super) split: CorpusSplit,
    /// What it runs: the bytes its content digest covers.
    pub(super) body: Body<'a>,
}

impl<'a> ItemFacts<'a> {
    /// `item`'s facts.
    #[inline]
    pub(super) const fn of(item: &'a Item) -> ItemFacts<'a> {
        ItemFacts {
            id: item.id(),
            kind: item.kind(),
            split: item.split(),
            body: match item.body() {
                ItemBody::Trace(bytes) => Body::Trace(bytes),
                ItemBody::Lane(s) => Body::Lane(s),
            },
        }
    }

    #[inline]
    fn content(&self) -> Digest {
        match self.body {
            Body::Trace(bytes) => Digest::of(GOLDEN_TRACE_DOMAIN, &[bytes]),
            Body::Lane(s) => s.digest(),
        }
    }
}

/// What the shipped baseline did on one item, measured now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Measured {
    /// The item measured.
    pub(super) id: String,
    /// What the shipped code did there.
    pub(super) baseline: Baseline,
    /// The policy's admission held for the shipped code on the item.
    pub(super) admitted: bool,
}

/// What a policy binds, in descriptor order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Terms {
    stranded_light: ArtifactId,
    lane_fidelity: ArtifactId,
    delta: Duration,
    plan_format: u8,
    plan_len: usize,
    max_gap_us: u32,
    min_period_us: u32,
    max_period_us: u32,
    interpreter: &'static str,
    plan_artifact: &'static str,
    lane_model: &'static str,
    scenario_format: u8,
    generator: u32,
    corpus: Digest,
    train: usize,
    held_out: usize,
    items: Vec<ItemPin>,
    baseline_code: &'static str,
    grid: Vec<SchedulePlan>,
    rounds: u8,
    distinct: usize,
    proposal_bytes: usize,
    distinct_by: &'static [u8],
    selection: &'static str,
    held_out_rule: &'static str,
    useful: &'static str,
    admission: &'static str,
    bases: Vec<(&'static str, Replace)>,
    schema: ArtifactId,
    profile: ArtifactId,
    grade_cap: &'static str,
}

#[inline]
const fn wide(n: usize) -> u64 {
    n as u64
}

impl Terms {
    #[inline]
    fn v1() -> Terms {
        let mut grid = Vec::with_capacity(GRID_V1.len());
        let mut gi = 0;
        while gi < GRID_V1.len() {
            let (split, wake) = GRID_V1[gi];
            if let Ok(plan) = SchedulePlan::new(split, wake) {
                grid.push(plan);
            }
            gi += 1;
        }
        Terms {
            stranded_light: NoStrandedLight::artifact(),
            lane_fidelity: LaneFidelity::artifact(),
            delta: PROBE_DELTA,
            plan_format: PLAN_FORMAT,
            plan_len: PLAN_LEN,
            max_gap_us: MAX_GAP_US,
            min_period_us: MIN_PERIOD_US,
            max_period_us: MAX_PERIOD_US,
            interpreter: PLAN_INTERPRETER,
            plan_artifact: PLAN_ARTIFACT_NAME,
            lane_model: LANE_MODEL,
            scenario_format: SCENARIO_FORMAT,
            generator: GENERATOR_VERSION,
            corpus: CORPUS_V1_DIGEST,
            train: TRAIN_COUNT,
            held_out: HELD_OUT_COUNT,
            items: ITEMS_V1.to_vec(),
            baseline_code: SHIPPED_CODE_NAME,
            grid,
            rounds: MAX_ROUNDS,
            distinct: MAX_DISTINCT,
            proposal_bytes: MAX_PROPOSAL_BYTES,
            distinct_by: PLAN_DOMAIN,
            selection: SELECTION_RULE,
            held_out_rule: HELD_OUT_RULE,
            useful: USEFUL_RULE,
            admission: ADMISSION_RULE,
            bases: BASES_V1.to_vec(),
            schema: schema_artifact(),
            profile: profile_artifact(),
            grade_cap: grade_name(GRADE_CAP),
        }
    }

    #[inline]
    fn descriptor(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(8192);
        e.artifact(&self.stranded_light);
        e.artifact(&self.lane_fidelity);
        e.duration(self.delta);
        e.u8(self.plan_format);
        e.u64(wide(self.plan_len));
        e.u32(self.max_gap_us);
        e.u32(self.min_period_us);
        e.u32(self.max_period_us);
        e.str(self.interpreter);
        e.str(self.plan_artifact);
        e.str(self.lane_model);
        e.u8(self.scenario_format);
        e.u32(self.generator);
        e.digest(&self.corpus);
        e.u64(wide(self.train));
        e.u64(wide(self.held_out));
        e.u64(wide(self.items.len()));
        let mut ii = 0;
        while ii < self.items.len() {
            let i = &self.items[ii];
            e.str(i.id);
            e.u8(i.kind.tag());
            e.u64(i.kind.seed());
            let stratum = match i.kind {
                ItemKind::Generated { stratum, .. } => Some(stratum),
                ItemKind::GoldenTrace | ItemKind::M3Story => None,
            };
            e.opt(stratum, |e, s| e.str(s.name()));
            e.u8(i.split.tag());
            e.digest(&i.content);
            ii += 1;
        }
        e.str(self.baseline_code);
        e.u64(wide(self.items.len()));
        let mut ii = 0;
        while ii < self.items.len() {
            let i = &self.items[ii];
            let b = &i.baseline;
            e.str(i.id);
            e.u64(b.ticks);
            e.u64(b.presented);
            e.u64(b.wasted);
            e.opt(b.lane, |e, v| e.u64(v));
            e.digest(&b.chain);
            ii += 1;
        }
        e.u64(wide(self.grid.len()));
        let mut gi = 0;
        while gi < self.grid.len() {
            e.raw(&self.grid[gi].encode());
            gi += 1;
        }
        e.u8(self.rounds);
        e.u64(wide(self.distinct));
        e.u64(wide(self.proposal_bytes));
        e.bytes(self.distinct_by);
        let rules = [
            self.selection,
            self.held_out_rule,
            self.useful,
            self.admission,
        ];
        let mut ri = 0;
        while ri < rules.len() {
            e.str(rules[ri]);
            ri += 1;
        }
        e.u64(wide(self.bases.len()));
        let mut bi = 0;
        while bi < self.bases.len() {
            let (code, how) = self.bases[bi];
            e.str(code);
            e.u8(how.tag());
            e.str(how.rule());
            bi += 1;
        }
        e.artifact(&self.schema);
        e.artifact(&self.profile);
        e.str(self.grade_cap);
        e.finish()
    }
}

/// The acceptance policy `momentum-glow.m4-acceptance` v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptancePolicy {
    id: ArtifactId,
    terms: Terms,
}

impl AcceptancePolicy {
    /// The policy v1 from this module's constants, unchecked: a search uses [`Self::pinned`].
    #[inline]
    #[must_use]
    pub fn v1() -> AcceptancePolicy {
        Self::of(Terms::v1())
    }

    #[inline]
    fn of(terms: Terms) -> AcceptancePolicy {
        let id = ArtifactId::new(
            ArtifactKind::Policy,
            POLICY_NAME,
            POLICY_VERSION,
            &terms.descriptor(),
        );
        AcceptancePolicy { id, terms }
    }

    /// [`Self::v1`], returned only when its digest is the pin, generator v1's corpus is the one it
    /// binds and the shipped baseline, measured from `origin` (the library reads no clock, and
    /// the origin changes no value), gives every pinned value and is still admitted.
    ///
    /// The process checks once, from the first caller's `origin`, and every later call is that
    /// answer: nothing a check reads changes while the process runs.
    /// # Errors
    /// [`PolicyRefusal::PolicyMutated`], then [`PolicyRefusal::CorpusDrift`], then
    /// [`PolicyRefusal::BaselineDrift`]: the first check that fails, in that order.
    #[inline]
    pub fn pinned(origin: Instant) -> Result<AcceptancePolicy, PolicyRefusal> {
        static PINNED: std::sync::OnceLock<Result<AcceptancePolicy, PolicyRefusal>> =
            std::sync::OnceLock::new();
        PINNED.get_or_init(|| Self::checked(origin)).clone()
    }

    /// [`Self::pinned`]'s checks, run.
    #[inline]
    fn checked(origin: Instant) -> Result<AcceptancePolicy, PolicyRefusal> {
        let policy = Self::v1();
        policy.check_pin()?;
        let Ok(corpus) = Corpus::generate() else {
            return Err(PolicyRefusal::CorpusDrift {
                item: policy.refused_item(),
            });
        };
        let items = corpus.items();
        let mut facts = Vec::with_capacity(items.len());
        let mut ii = 0;
        while ii < items.len() {
            facts.push(ItemFacts::of(&items[ii]));
            ii += 1;
        }
        policy.check_corpus(&facts)?;
        policy.check_baseline(&measure(&facts, origin))?;
        Ok(policy)
    }

    /// Its artifact.
    #[inline]
    #[must_use]
    pub const fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// The corpus items with their baseline, in corpus order.
    #[inline]
    #[must_use]
    pub const fn items(&self) -> &[ItemPin] {
        self.terms.items.as_slice()
    }

    /// The baseline's `T`, `P` and `Z` summed over `split`.
    #[inline]
    #[must_use]
    pub const fn totals(&self, split: CorpusSplit) -> Totals {
        let items = self.terms.items.as_slice();
        let mut t = Totals {
            ticks: 0,
            presented: 0,
            wasted: 0,
        };
        let mut i = 0;
        while i < items.len() {
            let pin = items[i];
            if matches!(
                (pin.split, split),
                (CorpusSplit::Train, CorpusSplit::Train)
                    | (CorpusSplit::HeldOut, CorpusSplit::HeldOut)
            ) {
                t.ticks = t.ticks.saturating_add(pin.baseline.ticks);
                t.presented = t.presented.saturating_add(pin.baseline.presented);
                t.wasted = t.wasted.saturating_add(pin.baseline.wasted);
            }
            i += 1;
        }
        t
    }

    /// The grid, in [`GRID_V1`]'s order.
    #[inline]
    #[must_use]
    pub const fn grid(&self) -> &[SchedulePlan] {
        self.terms.grid.as_slice()
    }

    /// The budgets.
    #[inline]
    #[must_use]
    pub const fn budgets(&self) -> Budgets {
        Budgets {
            rounds: self.terms.rounds,
            distinct: self.terms.distinct,
            proposal_bytes: self.terms.proposal_bytes,
        }
    }

    /// The codes a plan may replace, by artifact name, and how.
    #[inline]
    #[must_use]
    pub const fn bases(&self) -> &[(&'static str, Replace)] {
        self.terms.bases.as_slice()
    }

    /// How a plan may replace the live code `live`.
    #[inline]
    #[must_use]
    pub const fn base(&self, live: Sched) -> Option<Replace> {
        #[inline]
        const fn str_eq(a: &str, b: &str) -> bool {
            let (ab, bb) = (a.as_bytes(), b.as_bytes());
            if ab.len() != bb.len() {
                return false;
            }
            let mut i = 0;
            while i < ab.len() {
                if ab[i] != bb[i] {
                    return false;
                }
                i += 1;
            }
            true
        }
        let Some(code) = live.prebuilt() else {
            return None;
        };
        let target = code.artifact_name();
        let bases = self.terms.bases.as_slice();
        let mut bi = 0;
        while bi < bases.len() {
            let (name, how) = bases[bi];
            if str_eq(name, target) {
                return Some(how);
            }
            bi += 1;
        }
        None
    }

    /// The grade a result under this policy can carry, at most.
    #[inline]
    #[must_use]
    pub const fn grade_cap(&self) -> EvidenceGrade {
        GRADE_CAP
    }

    /// Checks that its digest is [`M4_POLICY_V1_DIGEST`].
    #[inline]
    pub(super) const fn check_pin(&self) -> Result<(), PolicyRefusal> {
        let a = self.id.digest.0;
        let b = M4_POLICY_V1_DIGEST.0;
        let mut i = 0;
        while i < 32 {
            if a[i] != b[i] {
                return Err(PolicyRefusal::PolicyMutated {
                    found: self.id.digest,
                    pinned: M4_POLICY_V1_DIGEST,
                });
            }
            i += 1;
        }
        Ok(())
    }

    /// Checks that `found` is the policy's corpus.
    #[inline]
    pub(super) fn check_corpus(&self, found: &[ItemFacts<'_>]) -> Result<(), PolicyRefusal> {
        #[inline]
        fn corpus_drift(item: &str) -> PolicyRefusal {
            PolicyRefusal::CorpusDrift {
                item: item.to_owned(),
            }
        }
        let mut k = 0;
        while k < self.terms.items.len() {
            let pin = &self.terms.items[k];
            let Some(f) = found.get(k) else {
                return Err(corpus_drift(pin.id));
            };
            if f.id != pin.id
                || f.kind != pin.kind
                || f.split != pin.split
                || f.content() != pin.content
            {
                return Err(corpus_drift(pin.id));
            }
            k += 1;
        }
        if let Some(extra) = found.get(self.terms.items.len()) {
            return Err(corpus_drift(extra.id));
        }
        let mut train_count = 0usize;
        let mut held_out_count = 0usize;
        let mut golden = None;
        let mut parts = Vec::with_capacity(found.len());
        let mut fi = 0;
        while fi < found.len() {
            let f = &found[fi];
            match f.split {
                CorpusSplit::Train => train_count += 1,
                CorpusSplit::HeldOut => held_out_count += 1,
            }
            match f.body {
                Body::Trace(bytes) => {
                    if golden.is_none() {
                        golden = Some(Digest::of(GOLDEN_TRACE_DOMAIN, &[bytes]));
                    }
                }
                Body::Lane(s) => {
                    parts.push(item_part(f.kind, f.split, &s.encode()));
                }
            }
            fi += 1;
        }
        if train_count != self.terms.train || held_out_count != self.terms.held_out {
            return Err(corpus_drift(CORPUS_FOLD));
        }
        match golden {
            Some(golden) if corpus_digest(&golden, &parts) == self.terms.corpus => Ok(()),
            _ => Err(corpus_drift(CORPUS_FOLD)),
        }
    }

    /// Checks that `measured` is one admitted row per item.
    #[inline]
    pub(super) fn check_baseline(&self, measured: &[Measured]) -> Result<(), PolicyRefusal> {
        #[inline]
        fn baseline_drift(item: &str, field: BaselineField) -> PolicyRefusal {
            PolicyRefusal::BaselineDrift {
                item: item.to_owned(),
                field,
            }
        }
        #[inline]
        const fn class_admitted(m: &Measured, pin: &ItemPin) -> Option<BaselineField> {
            if !m.admitted && !matches!(pin.kind, ItemKind::GoldenTrace) {
                Some(BaselineField::Admitted)
            } else {
                None
            }
        }
        #[inline]
        const fn class_golden_head(m: &Measured, pin: &ItemPin) -> Option<BaselineField> {
            if !m.admitted && matches!(pin.kind, ItemKind::GoldenTrace) {
                Some(BaselineField::GoldenHead)
            } else {
                None
            }
        }
        #[inline]
        const fn class_tpz(m: &Measured, pin: &ItemPin) -> Option<BaselineField> {
            let (b, p) = (&m.baseline, &pin.baseline);
            if b.ticks != p.ticks {
                Some(BaselineField::Ticks)
            } else if b.presented != p.presented {
                Some(BaselineField::Presented)
            } else if b.wasted != p.wasted {
                Some(BaselineField::Wasted)
            } else {
                None
            }
        }
        #[inline]
        const fn class_digests(m: &Measured, pin: &ItemPin) -> Option<BaselineField> {
            let (b, p) = (&m.baseline, &pin.baseline);
            let lane_same = match (b.lane, p.lane) {
                (Some(x), Some(y)) => x == y,
                (None, None) => true,
                _ => false,
            };
            if !lane_same {
                return Some(BaselineField::LaneDigest);
            }
            let mut i = 0;
            while i < 32 {
                if b.chain.0[i] != p.chain.0[i] {
                    return Some(BaselineField::Chain);
                }
                i += 1;
            }
            None
        }
        let items = &self.terms.items;
        let mut k = 0;
        while k < items.len() {
            let pin = &items[k];
            match measured.get(k) {
                Some(m) if m.id == pin.id => {}
                _ => return Err(baseline_drift(pin.id, BaselineField::Row)),
            }
            k += 1;
        }
        if let Some(extra) = measured.get(items.len()) {
            return Err(baseline_drift(&extra.id, BaselineField::Row));
        }
        let classes: [fn(&Measured, &ItemPin) -> Option<BaselineField>; 4] =
            [class_admitted, class_golden_head, class_tpz, class_digests];
        let mut ci = 0;
        while ci < classes.len() {
            let class = classes[ci];
            let len = items.len().min(measured.len());
            let mut k = 0;
            while k < len {
                let pin = &items[k];
                if let Some(field) = class(&measured[k], pin) {
                    return Err(baseline_drift(pin.id, field));
                }
                k += 1;
            }
            ci += 1;
        }
        Ok(())
    }

    #[inline]
    fn refused_item(&self) -> String {
        let mut ii = 0;
        while ii < self.terms.items.len() {
            let i = &self.terms.items[ii];
            let refused = match i.kind {
                ItemKind::Generated { seed, .. } => scenario(seed).is_err(),
                ItemKind::M3Story => s0().validate().is_err(),
                ItemKind::GoldenTrace => false,
            };
            if refused {
                return i.id.to_owned();
            }
            ii += 1;
        }
        CORPUS_FOLD.to_owned()
    }
}

/// A field of the descriptor as the test-only mutation hook alters it, in descriptor order.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    StrandedLight,
    LaneFidelity,
    Delta,
    PlanFormat,
    PlanLen,
    MaxGap,
    MinPeriod,
    MaxPeriod,
    Interpreter,
    PlanArtifact,
    LaneModel,
    ScenarioFormat,
    GeneratorVersion,
    CorpusDigest,
    TrainCount,
    HeldOutCount,
    ItemDropped,
    ItemOrder,
    ItemId,
    ItemKind,
    ItemSeed,
    ItemStratum,
    ItemSplit,
    ItemContent,
    BaselineCode,
    BaselineTicks,
    BaselinePresented,
    BaselineWasted,
    BaselineLane,
    BaselineGoldenLane,
    BaselineChain,
    GridDropped,
    GridOrder,
    GridPlan,
    Rounds,
    Distinct,
    ProposalBytes,
    DistinctBy,
    Selection,
    HeldOut,
    Useful,
    Admission,
    BaseDropped,
    BaseCode,
    BaseMode,
    Schema,
    Profile,
    GradeCap,
}

#[cfg(test)]
impl Field {
    /// Every field, in descriptor order.
    pub(super) const ALL: [Field; 48] = [
        Field::StrandedLight,
        Field::LaneFidelity,
        Field::Delta,
        Field::PlanFormat,
        Field::PlanLen,
        Field::MaxGap,
        Field::MinPeriod,
        Field::MaxPeriod,
        Field::Interpreter,
        Field::PlanArtifact,
        Field::LaneModel,
        Field::ScenarioFormat,
        Field::GeneratorVersion,
        Field::CorpusDigest,
        Field::TrainCount,
        Field::HeldOutCount,
        Field::ItemDropped,
        Field::ItemOrder,
        Field::ItemId,
        Field::ItemKind,
        Field::ItemSeed,
        Field::ItemStratum,
        Field::ItemSplit,
        Field::ItemContent,
        Field::BaselineCode,
        Field::BaselineTicks,
        Field::BaselinePresented,
        Field::BaselineWasted,
        Field::BaselineLane,
        Field::BaselineGoldenLane,
        Field::BaselineChain,
        Field::GridDropped,
        Field::GridOrder,
        Field::GridPlan,
        Field::Rounds,
        Field::Distinct,
        Field::ProposalBytes,
        Field::DistinctBy,
        Field::Selection,
        Field::HeldOut,
        Field::Useful,
        Field::Admission,
        Field::BaseDropped,
        Field::BaseCode,
        Field::BaseMode,
        Field::Schema,
        Field::Profile,
        Field::GradeCap,
    ];
}

#[cfg(test)]
fn flip(d: &mut Digest) {
    d.0[0] ^= 1;
}

#[cfg(test)]
impl Terms {
    fn mutate(&mut self, field: Field) {
        let item = &mut self.items[1];
        match field {
            Field::StrandedLight => flip(&mut self.stranded_light.digest),
            Field::LaneFidelity => flip(&mut self.lane_fidelity.digest),
            Field::Delta => self.delta += Duration::from_nanos(1),
            Field::PlanFormat => self.plan_format += 1,
            Field::PlanLen => self.plan_len += 1,
            Field::MaxGap => self.max_gap_us += 1,
            Field::MinPeriod => self.min_period_us -= 1,
            Field::MaxPeriod => self.max_period_us += 1,
            Field::Interpreter => self.interpreter = "plan interpreter v2",
            Field::PlanArtifact => self.plan_artifact = "momentum-glow.schedule-plan-v2",
            Field::LaneModel => self.lane_model = "aterm frame lane v2",
            Field::ScenarioFormat => self.scenario_format += 1,
            Field::GeneratorVersion => self.generator += 1,
            Field::CorpusDigest => flip(&mut self.corpus),
            Field::TrainCount => self.train -= 1,
            Field::HeldOutCount => self.held_out += 1,
            Field::ItemDropped => {
                self.items.remove(5);
            }
            Field::ItemOrder => self.items.swap(1, 2),
            Field::ItemId => item.id = "train-99",
            Field::ItemKind => item.kind = ItemKind::M3Story,
            Field::ItemSeed => {
                item.kind = generated(item.kind.seed() + 100, Stratum::CaretAtKeys);
            }
            Field::ItemStratum => item.kind = generated(item.kind.seed(), Stratum::Plain),
            Field::ItemSplit => item.split = CorpusSplit::HeldOut,
            Field::ItemContent => flip(&mut item.content),
            Field::BaselineCode => self.baseline_code = DEMO_CODE_NAME,
            Field::BaselineTicks => item.baseline.ticks += 1,
            Field::BaselinePresented => item.baseline.presented += 1,
            Field::BaselineWasted => item.baseline.wasted += 1,
            Field::BaselineLane => item.baseline.lane = None,
            Field::BaselineGoldenLane => self.items[0].baseline.lane = Some(0),
            Field::BaselineChain => flip(&mut item.baseline.chain),
            Field::GridDropped => {
                self.grid.pop();
            }
            Field::GridOrder => self.grid.swap(0, 1),
            Field::GridPlan => {
                self.grid[2] = SchedulePlan::new(Split::Shipped, Wake::Poll { period_us: 16_668 })
                    .expect("fixture: an in-range plan");
            }
            Field::Rounds => self.rounds += 1,
            Field::Distinct => self.distinct += 1,
            Field::ProposalBytes => self.proposal_bytes += 1,
            Field::DistinctBy => self.distinct_by = b"aterm-aos-m4-plan-v2\0",
            Field::Selection => self.selection = "m4-selection v2",
            Field::HeldOut => self.held_out_rule = "m4-held-out v2",
            Field::Useful => self.useful = "m4-useful v2",
            Field::Admission => self.admission = "m4-admission v2",
            Field::BaseDropped => {
                self.bases.pop();
            }
            Field::BaseCode => self.bases[0].0 = "momentum-glow.other",
            Field::BaseMode => self.bases[0].1 = Replace::Repair,
            Field::Schema => flip(&mut self.schema.digest),
            Field::Profile => flip(&mut self.profile.digest),
            Field::GradeCap => self.grade_cap = "Proved",
        }
    }
}

#[cfg(test)]
impl AcceptancePolicy {
    /// v1 with `field` altered.
    pub(super) fn mutated(field: Field) -> AcceptancePolicy {
        let mut terms = Terms::v1();
        terms.mutate(field);
        Self::of(terms)
    }
}

/// The shipped baseline on every item of `facts`.
#[inline]
#[must_use]
pub(super) fn measure(facts: &[ItemFacts<'_>], origin: Instant) -> Vec<Measured> {
    let mut out = Vec::with_capacity(facts.len());
    let mut fi = 0;
    while fi < facts.len() {
        let f = &facts[fi];
        let (baseline, admitted) = match f.body {
            Body::Lane(s) => measure_lane(s, origin),
            Body::Trace(bytes) => measure_trace(bytes, origin),
        };
        out.push(Measured {
            id: f.id.to_owned(),
            baseline,
            admitted,
        });
        fi += 1;
    }
    out
}

const UNMEASURED: Baseline = Baseline {
    ticks: 0,
    presented: 0,
    wasted: 0,
    lane: None,
    chain: Digest([0; 32]),
};

#[inline]
fn measure_lane(s: &Scenario, origin: Instant) -> (Baseline, bool) {
    let mut h = MomentumHost::default();
    h.attach_sched(
        origin,
        Sched::Prebuilt(MomentumCode::Shipped),
        BASELINE_BUILD,
        1,
        MOMENTUM_QUEUE_CAP,
        MOMENTUM_MAX_EVENTS,
    );
    let r = run(&mut h, s, origin);
    let fidelity = LaneFidelity::new().judge(&r);
    let HostMode::Hosted(hd) = &h.mode else {
        return (UNMEASURED, false);
    };
    let b = &hd.boundary;
    let (Some(checkpoint), Some(log)) = (b.checkpoint(), b.log()) else {
        return (UNMEASURED, false);
    };
    let Ok(start) = checkpoint.restore(b.schema(), b.identity()) else {
        return (UNMEASURED, false);
    };
    let (replayed, verdict) = NoStrandedLight::new().replay_judged(
        b.schema(),
        &b.pair().code,
        start,
        log.inputs(),
        log.header().first_cursor,
        &mut NoEffects,
    );
    let reproduced = replayed.refused.is_none()
        && replayed.observation_chain == hd.live_chain()
        && replayed.state_digest == state_digest(b.schema(), hd.state());
    let c = r.counts();
    let baseline = Baseline {
        ticks: c.ticks,
        presented: c.presented,
        wasted: c.wasted(),
        lane: Some(r.fnv()),
        chain: replayed.observation_chain,
    };
    let admitted = r.fault().is_none()
        && fidelity.passed()
        && log.inputs() == r.calls()
        && reproduced
        && verdict.passed();
    (baseline, admitted)
}

#[inline]
fn measure_trace(bytes: &[u8], origin: Instant) -> (Baseline, bool) {
    let Ok(trace) = MomentumTrace::decode(bytes) else {
        return (UNMEASURED, false);
    };
    let Ok(read) = trace.read(origin) else {
        return (UNMEASURED, false);
    };
    let Ok(start) = read.checkpoint.restore(&read.schema, &trace.identity) else {
        return (UNMEASURED, false);
    };
    let code = PrebuiltMomentum::new(MomentumCode::Shipped, BASELINE_BUILD);
    let entered = code.entry(&read.schema, &start).is_ok();
    let replayed = replay(
        &read.schema,
        &code,
        start,
        read.log.inputs(),
        read.log.header().first_cursor,
        &mut NoEffects,
    );
    let verdict = NoStrandedLight::new().judge(&read.schema, read.log.inputs(), &replayed);
    let (mut ticks, mut presented) = (0u64, 0u64);
    let mut glass = Enc::with_capacity(512);
    encode_glass(&mut glass, &MomentumGlowFrame::default(), &[], &[]);
    let mut drawn = Enc::with_capacity(512);
    let obs_list = &replayed.observations;
    let mut oi = 0;
    while oi < obs_list.len() {
        let o = &obs_list[oi].1;
        if let MomentumObservation::Frame { .. } = o {
            ticks += 1;
            drawn.clear();
            read.schema.encode_observation(o, &mut drawn);
            if drawn.as_bytes() != glass.as_bytes() {
                presented += 1;
                core::mem::swap(&mut glass, &mut drawn);
            }
        }
        oi += 1;
    }
    let baseline = Baseline {
        ticks,
        presented,
        wasted: ticks.saturating_sub(presented),
        lane: None,
        chain: replayed.observation_chain,
    };
    let admitted = entered
        && replayed.refused.is_none()
        && replayed.state_digest == trace.head_state
        && replayed.observation_chain == trace.head_chain
        && verdict.passed();
    (baseline, admitted)
}
