// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The provenance bundle of an admitted Aos M4 candidate, and its verification (design §10.7).

use core::fmt;

use aterm_aos::evidence::TESTED_ASSUMPTIONS;
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, Digest};

use super::corpus::{CorpusSplit, ItemKind, Stratum};
use super::fidelity::{LaneFidelity, ORACLE};
use super::lane::LANE_MODEL;
use super::m4::{Compared, Evaluator, Sums, sums};
use super::plan::{PLAN_INTERPRETER, SchedulePlan};
use super::policy::{MAX_DISTINCT, MAX_ROUNDS, Replace, Totals};
use super::search::{GridProposer, Mode, Proposer, ProposerId, SearchStats};
use super::{MomentumCode, NoStrandedLight, Sched, profile_artifact, schema_artifact};

/// The bytes that open a bundle: `AOSCAND\0` then the format byte, 1.
pub const BUNDLE_MAGIC: [u8; 9] = *b"AOSCAND\0\x01";

/// The domain of the seal that closes a bundle.
pub const BUNDLE_DOMAIN: &[u8] = b"aterm-aos-m4-candidate-bundle-v1\0";

/// The grade a bundle carries.
pub const BUNDLE_GRADE: &str = "Tested";

/// What a plan's result rests on beyond [`TESTED_ASSUMPTIONS`] (design §10.8).
pub const PLAN_ASSUMPTIONS: [&str; 5] = [
    "the lane models the host: one-shot wakes are exact in the lane, while the window host rounds its coarse slot",
    "the plan-only presentation guard holds where the fallback cannot see a change",
    "the engine's f32 settle forecast holds under the profile",
    "the plan interpreter is tested, not proved",
    "the held-out seeds are visible in source",
];

/// The toolchain line a bundle declares until its caller declares one.
pub const UNDECLARED: &str = "undeclared";

/// The most rows a bundle decodes (a bound on what a hostile count can allocate).
pub const MAX_ROWS: u64 = 256;

/// The most timings a bundle decodes.
pub const MAX_TIMINGS: u64 = 16;

const MAX_ASSUMPTIONS: u64 = 16;

const SEAL_LEN: usize = 32;

/// A verdict as a row records it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowVerdict {
    /// The check did not run.
    NotRun,
    /// Every law held.
    Pass,
    /// `law` failed first, at `cursor`.
    Fail {
        /// The law (a stable name).
        law: String,
        /// Where it failed: the input cursor, or the tick ordinal for lane fidelity.
        cursor: u64,
    },
}

impl RowVerdict {
    /// A failure of `law` at `cursor`.
    #[inline]
    #[must_use]
    pub fn fail(law: &str, cursor: u64) -> RowVerdict {
        RowVerdict::Fail {
            law: law.to_owned(),
            cursor,
        }
    }

    /// Whether it is [`RowVerdict::Pass`].
    #[inline]
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, RowVerdict::Pass)
    }
}

/// One corpus item's evidence for the candidate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    /// The item's id.
    pub item: String,
    /// Its split.
    pub split: CorpusSplit,
    /// Its kind.
    pub kind: ItemKind,
    /// Whether the run was made: the golden trace read and the plan entered on its checkpoint, or
    /// the host logged every call and its replayed log reproduced the live head.
    pub run: RowVerdict,
    /// `NoStrandedLight` on the replay.
    pub stranded_light: RowVerdict,
    /// Lane fidelity on the lane run ([`RowVerdict::NotRun`] for the golden trace).
    pub lane_fidelity: RowVerdict,
    /// The lane's FNV-1a digest (none for the golden trace).
    pub lane: Option<u64>,
    /// The replay's observation chain.
    pub chain: Digest,
    /// `T`.
    pub ticks: u64,
    /// `P`.
    pub presented: u64,
    /// `Z = T − P`.
    pub wasted: u64,
    /// How often the plan-only presentation guard held the cadence the plan released.
    pub firings: Option<u64>,
}

impl Row {
    /// Its `T`, `P` and `Z`.
    #[inline]
    #[must_use]
    pub const fn totals(&self) -> Totals {
        Totals {
            ticks: self.ticks,
            presented: self.presented,
            wasted: self.wasted,
        }
    }
}

/// What the selection did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionPart {
    /// Search or on-demand.
    pub mode: Mode,
    /// The search's counts ([`SearchStats::ONE`] on demand).
    pub stats: SearchStats,
    /// How often the held-out split was read for this result: once.
    pub held_out_looks: u8,
    /// The training split.
    pub train: Compared,
    /// The held-out split.
    pub held_out: Compared,
    /// `S0` alone.
    pub s0: Compared,
    /// Useful by the policy's rule.
    pub useful: bool,
    /// `1 − ΣT_c / ΣT_B` over the held-out split, in parts per million.
    pub gain_ppm: i64,
    /// The same for `S0` alone.
    pub s0_gain_ppm: i64,
}

/// The live head a publication found, as its witness names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PublishedHead {
    /// The task identity's digest.
    pub identity: Digest,
    /// The revision.
    pub revision: u64,
    /// The branch.
    pub branch: u64,
    /// The live state's digest.
    pub state: Digest,
    /// The next input's cursor.
    pub input_cursor: u64,
    /// The next effect's cursor.
    pub effect_cursor: u64,
}

/// Publication metadata from the witness, checked offline for structural consistency only.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Publication {
    /// The paused head the live code's replay reproduced.
    pub head: PublishedHead,
    /// The revision the commit minted.
    pub revision: u64,
    /// The live code replaced.
    pub live_code: ArtifactId,
    /// Its verdict on the live log.
    pub live_verdict: RowVerdict,
    /// The digest of the replayed inputs.
    pub trace: Digest,
    /// The replayed cursor range `[from, to)`.
    pub from: u64,
    /// See `from`.
    pub to: u64,
    /// The log epoch.
    pub log_epoch: u64,
    /// The ancestor checkpoint's digest.
    pub ancestor: Digest,
    /// The code published.
    pub candidate_code: ArtifactId,
    /// Its verdict on the live log.
    pub candidate_verdict: RowVerdict,
    /// Its replay's observation chain.
    pub candidate_chain: Digest,
    /// The state its replay reached.
    pub candidate_state: Digest,
}

/// One lab timing: wall time the lab measured, labelled.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Timing {
    /// What was measured.
    pub label: String,
    /// How long it took, in ns, as the lab's clock read it.
    pub measured_ns: u64,
}

/// A bundle's parts as its bytes say, verified or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncheckedBundle {
    /// The acceptance policy.
    pub policy: ArtifactId,
    /// The base: the shipped code the baseline is measured on, in `build`.
    pub base: ArtifactId,
    /// The candidate: the planned code, in `build`.
    pub candidate: ArtifactId,
    /// The plan's bytes.
    pub plan: Vec<u8>,
    /// Who proposed it.
    pub proposer: ProposerId,
    /// The round it was proposed in.
    pub round: u8,
    /// Its index among that round's proposals.
    pub index: u64,
    /// The corrected contract.
    pub stranded_light: ArtifactId,
    /// The second contract.
    pub lane_fidelity: ArtifactId,
    /// The interpreter.
    pub interpreter: String,
    /// The lane model.
    pub lane: String,
    /// Lane fidelity's oracle.
    pub oracle: String,
    /// The corpus digest.
    pub corpus: Digest,
    /// The schema.
    pub schema: ArtifactId,
    /// The profile.
    pub profile: ArtifactId,
    /// The build the evaluator ran in.
    pub build: String,
    /// The toolchain line, as declared.
    pub toolchain: String,
    /// One row per corpus item, in policy order.
    pub rows: Vec<Row>,
    /// The selection.
    pub selection: SelectionPart,
    /// The publication, once published.
    pub publication: Option<Publication>,
    /// Lab timings, never scored.
    pub timings: Vec<Timing>,
    /// The grade.
    pub grade: String,
    /// [`TESTED_ASSUMPTIONS`].
    pub tested_assumptions: Vec<String>,
    /// [`PLAN_ASSUMPTIONS`].
    pub plan_assumptions: Vec<String>,
}

/// A part of a bundle, as a forgery names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BundleField {
    /// The policy.
    Policy,
    /// The build.
    Build,
    /// The base.
    Base,
    /// The plan's bytes.
    Plan,
    /// The candidate's identity.
    Candidate,
    /// The corrected contract.
    StrandedLightContract,
    /// The lane-fidelity contract.
    LaneFidelityContract,
    /// The interpreter.
    Interpreter,
    /// The lane model.
    Lane,
    /// The oracle.
    Oracle,
    /// The corpus digest.
    Corpus,
    /// The schema.
    Schema,
    /// The profile.
    Profile,
    /// The grade.
    Grade,
    /// An assumption list.
    Assumptions,
    /// The proposer, round or index.
    Proposer,
    /// A row is missing, extra or another item's (id, split or kind).
    Row,
    /// A row's run.
    Run,
    /// A row's `NoStrandedLight` verdict.
    StrandedLightVerdict,
    /// A row's lane-fidelity verdict.
    LaneFidelityVerdict,
    /// A row's lane digest.
    LaneDigest,
    /// A row's observation chain.
    Chain,
    /// A row's `T`.
    Ticks,
    /// A row's `P`.
    Presented,
    /// A row's `Z`.
    Wasted,
    /// A row's fallback firings.
    Firings,
    /// The search's counts or the held-out looks.
    Stats,
    /// A sum.
    Sums,
    /// The usefulness.
    Useful,
    /// A gain.
    Gain,
    /// The timings.
    Timings,
    /// The publication.
    Publication,
}

impl BundleField {
    /// Its name in a refusal.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            BundleField::Policy => "policy",
            BundleField::Build => "build",
            BundleField::Base => "base",
            BundleField::Plan => "plan",
            BundleField::Candidate => "candidate",
            BundleField::StrandedLightContract => "contract",
            BundleField::LaneFidelityContract => "lane-fidelity contract",
            BundleField::Interpreter => "interpreter",
            BundleField::Lane => "lane model",
            BundleField::Oracle => "oracle",
            BundleField::Corpus => "corpus",
            BundleField::Schema => "schema",
            BundleField::Profile => "profile",
            BundleField::Grade => "grade",
            BundleField::Assumptions => "assumptions",
            BundleField::Proposer => "proposer",
            BundleField::Row => "row",
            BundleField::Run => "run",
            BundleField::StrandedLightVerdict => "NoStrandedLight verdict",
            BundleField::LaneFidelityVerdict => "lane-fidelity verdict",
            BundleField::LaneDigest => "lane digest",
            BundleField::Chain => "observation chain",
            BundleField::Ticks => "T",
            BundleField::Presented => "P",
            BundleField::Wasted => "Z",
            BundleField::Firings => "fallback firings",
            BundleField::Stats => "counts",
            BundleField::Sums => "sums",
            BundleField::Useful => "useful",
            BundleField::Gain => "gain",
            BundleField::Timings => "timings",
            BundleField::Publication => "publication",
        }
    }
}

/// Why bytes are not a bundle the evaluator stands behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BundleRefusal {
    /// The bytes do not open with `AOSCAND\0`.
    Magic,
    /// They open with another format byte than [`BUNDLE_MAGIC`]'s.
    Format {
        /// The byte found.
        found: u8,
    },
    /// The seal does not name the bytes before it: the bundle was cut or altered.
    Seal {
        /// The digest the bundle closes with.
        sealed: Digest,
        /// The digest of its bytes.
        found: Digest,
    },
    /// A part does not decode, or bytes follow the seal's body.
    Wire(WireFault),
    /// A part is not what the pinned policy computes.
    Forged {
        /// The part.
        field: BundleField,
        /// The item whose row it is, for a row's part.
        row: Option<String>,
    },
}

impl fmt::Display for BundleRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleRefusal::Magic => f.write_str("the bytes are not a candidate bundle"),
            BundleRefusal::Format { found } => write!(
                f,
                "the bundle is format {found}; this build reads format {}",
                BUNDLE_MAGIC[8]
            ),
            BundleRefusal::Seal { sealed, found } => write!(
                f,
                "the bundle closes with the digest {}, but its bytes digest to {}: it was cut or altered",
                sealed.short(),
                found.short()
            ),
            BundleRefusal::Wire(w) => write!(f, "the bundle does not decode: {w}"),
            BundleRefusal::Forged { field, row: None } => write!(
                f,
                "the bundle is forged: its {} is not what the pinned policy computes",
                field.name()
            ),
            BundleRefusal::Forged {
                field,
                row: Some(item),
            } => write!(
                f,
                "the bundle is forged: the {} of the row for {item} is not what the pinned policy computes",
                field.name()
            ),
        }
    }
}

impl std::error::Error for BundleRefusal {}

#[inline]
fn forged(field: BundleField) -> BundleRefusal {
    BundleRefusal::Forged { field, row: None }
}

#[inline]
fn forged_row(field: BundleField, item: &str) -> BundleRefusal {
    BundleRefusal::Forged {
        field,
        row: Some(item.to_owned()),
    }
}

#[inline]
fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn encode_verdict(e: &mut Enc, v: &RowVerdict) {
    match v {
        RowVerdict::NotRun => e.u8(0),
        RowVerdict::Pass => e.u8(1),
        RowVerdict::Fail { law, cursor } => {
            e.u8(2);
            e.str(law);
            e.u64(*cursor);
        }
    }
}

fn decode_verdict(d: &mut Dec<'_>, field: &'static str) -> Result<RowVerdict, WireFault> {
    match d.u8(field)? {
        0 => Ok(RowVerdict::NotRun),
        1 => Ok(RowVerdict::Pass),
        2 => Ok(RowVerdict::Fail {
            law: d.str(field)?.to_owned(),
            cursor: d.u64(field)?,
        }),
        tag => Err(WireFault::BadTag { field, tag }),
    }
}

fn encode_totals(e: &mut Enc, t: &Totals) {
    e.u64(t.ticks);
    e.u64(t.presented);
    e.u64(t.wasted);
}

fn decode_totals(d: &mut Dec<'_>, field: &'static str) -> Result<Totals, WireFault> {
    Ok(Totals {
        ticks: d.u64(field)?,
        presented: d.u64(field)?,
        wasted: d.u64(field)?,
    })
}

fn encode_compared(e: &mut Enc, c: &Compared) {
    encode_totals(e, &c.candidate);
    encode_totals(e, &c.baseline);
}

fn decode_compared(d: &mut Dec<'_>, field: &'static str) -> Result<Compared, WireFault> {
    Ok(Compared {
        candidate: decode_totals(d, field)?,
        baseline: decode_totals(d, field)?,
    })
}

fn encode_kind(e: &mut Enc, k: ItemKind) {
    e.u8(k.tag());
    e.u64(k.seed());
    let stratum = match k {
        ItemKind::Generated { stratum, .. } => Some(stratum),
        ItemKind::GoldenTrace | ItemKind::M3Story => None,
    };
    e.opt(stratum, |e, s| e.str(s.name()));
}

fn decode_kind(d: &mut Dec<'_>) -> Result<ItemKind, WireFault> {
    const FIELD: &str = "row.kind";
    let tag = d.u8(FIELD)?;
    let seed = d.u64(FIELD)?;
    let stratum = d.opt(FIELD, |d| {
        let name = d.str(FIELD)?;
        Stratum::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or(WireFault::NonCanonical { field: FIELD })
    })?;
    match (tag, stratum) {
        (0, None) if seed == 0 => Ok(ItemKind::GoldenTrace),
        (1, None) if seed == 0 => Ok(ItemKind::M3Story),
        (2, Some(stratum)) => Ok(ItemKind::Generated { seed, stratum }),
        (0..=2, _) => Err(WireFault::NonCanonical { field: FIELD }),
        (tag, _) => Err(WireFault::BadTag { field: FIELD, tag }),
    }
}

pub(super) fn encode_row(e: &mut Enc, r: &Row) {
    e.str(&r.item);
    e.u8(r.split.tag());
    encode_kind(e, r.kind);
    encode_verdict(e, &r.run);
    encode_verdict(e, &r.stranded_light);
    encode_verdict(e, &r.lane_fidelity);
    e.opt(r.lane, |e, v| e.u64(v));
    e.digest(&r.chain);
    e.u64(r.ticks);
    e.u64(r.presented);
    e.u64(r.wasted);
    e.opt(r.firings, |e, v| e.u64(v));
}

pub(super) fn decode_row(d: &mut Dec<'_>) -> Result<Row, WireFault> {
    let item = d.str("row.item")?.to_owned();
    let split = match d.u8("row.split")? {
        0 => CorpusSplit::Train,
        1 => CorpusSplit::HeldOut,
        tag => {
            return Err(WireFault::BadTag {
                field: "row.split",
                tag,
            });
        }
    };
    Ok(Row {
        item,
        split,
        kind: decode_kind(d)?,
        run: decode_verdict(d, "row.run")?,
        stranded_light: decode_verdict(d, "row.stranded_light")?,
        lane_fidelity: decode_verdict(d, "row.lane_fidelity")?,
        lane: d.opt("row.lane", |d| d.u64("row.lane"))?,
        chain: d.digest("row.chain")?,
        ticks: d.u64("row.ticks")?,
        presented: d.u64("row.presented")?,
        wasted: d.u64("row.wasted")?,
        firings: d.opt("row.firings", |d| d.u64("row.firings"))?,
    })
}

fn encode_stats(e: &mut Enc, s: &SearchStats) {
    e.u8(s.rounds);
    for v in [
        s.proposed,
        s.oversized,
        s.duplicates,
        s.over_budget,
        s.distinct,
        s.admitted,
    ] {
        e.u64(v);
    }
}

fn decode_stats(d: &mut Dec<'_>) -> Result<SearchStats, WireFault> {
    const FIELD: &str = "selection.stats";
    Ok(SearchStats {
        rounds: d.u8(FIELD)?,
        proposed: d.u64(FIELD)?,
        oversized: d.u64(FIELD)?,
        duplicates: d.u64(FIELD)?,
        over_budget: d.u64(FIELD)?,
        distinct: d.u64(FIELD)?,
        admitted: d.u64(FIELD)?,
    })
}

fn encode_selection(e: &mut Enc, s: &SelectionPart) {
    e.u8(s.mode.tag());
    encode_stats(e, &s.stats);
    e.u8(s.held_out_looks);
    encode_compared(e, &s.train);
    encode_compared(e, &s.held_out);
    encode_compared(e, &s.s0);
    e.bool(s.useful);
    e.i64(s.gain_ppm);
    e.i64(s.s0_gain_ppm);
}

fn decode_selection(d: &mut Dec<'_>) -> Result<SelectionPart, WireFault> {
    let tag = d.u8("selection.mode")?;
    let mode = Mode::from_tag(tag).ok_or(WireFault::BadTag {
        field: "selection.mode",
        tag,
    })?;
    Ok(SelectionPart {
        mode,
        stats: decode_stats(d)?,
        held_out_looks: d.u8("selection.held_out_looks")?,
        train: decode_compared(d, "selection.train")?,
        held_out: decode_compared(d, "selection.held_out")?,
        s0: decode_compared(d, "selection.s0")?,
        useful: d.bool("selection.useful")?,
        gain_ppm: d.i64("selection.gain")?,
        s0_gain_ppm: d.i64("selection.s0_gain")?,
    })
}

fn encode_publication(e: &mut Enc, p: &Publication) {
    let h = &p.head;
    e.digest(&h.identity);
    e.u64(h.revision);
    e.u64(h.branch);
    e.digest(&h.state);
    e.u64(h.input_cursor);
    e.u64(h.effect_cursor);
    e.u64(p.revision);
    e.artifact(&p.live_code);
    encode_verdict(e, &p.live_verdict);
    e.digest(&p.trace);
    e.u64(p.from);
    e.u64(p.to);
    e.u64(p.log_epoch);
    e.digest(&p.ancestor);
    e.artifact(&p.candidate_code);
    encode_verdict(e, &p.candidate_verdict);
    e.digest(&p.candidate_chain);
    e.digest(&p.candidate_state);
}

fn decode_publication(d: &mut Dec<'_>) -> Result<Publication, WireFault> {
    const FIELD: &str = "publication";
    Ok(Publication {
        head: PublishedHead {
            identity: d.digest(FIELD)?,
            revision: d.u64(FIELD)?,
            branch: d.u64(FIELD)?,
            state: d.digest(FIELD)?,
            input_cursor: d.u64(FIELD)?,
            effect_cursor: d.u64(FIELD)?,
        },
        revision: d.u64(FIELD)?,
        live_code: d.artifact(FIELD)?,
        live_verdict: decode_verdict(d, FIELD)?,
        trace: d.digest(FIELD)?,
        from: d.u64(FIELD)?,
        to: d.u64(FIELD)?,
        log_epoch: d.u64(FIELD)?,
        ancestor: d.digest(FIELD)?,
        candidate_code: d.artifact(FIELD)?,
        candidate_verdict: decode_verdict(d, FIELD)?,
        candidate_chain: d.digest(FIELD)?,
        candidate_state: d.digest(FIELD)?,
    })
}

fn decode_strs(d: &mut Dec<'_>, field: &'static str) -> Result<Vec<String>, WireFault> {
    let n = d.count(field, MAX_ASSUMPTIONS)?;
    (0..n).map(|_| Ok(d.str(field)?.to_owned())).collect()
}

impl UncheckedBundle {
    /// The bundle's bytes, sealed.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(8 * 1024);
        e.raw(&BUNDLE_MAGIC);
        e.artifact(&self.policy);
        e.artifact(&self.base);
        e.artifact(&self.candidate);
        e.bytes(&self.plan);
        e.str(&self.proposer.name);
        e.u32(self.proposer.version);
        e.digest(&self.proposer.config);
        e.u8(self.round);
        e.u64(self.index);
        e.artifact(&self.stranded_light);
        e.artifact(&self.lane_fidelity);
        e.str(&self.interpreter);
        e.str(&self.lane);
        e.str(&self.oracle);
        e.digest(&self.corpus);
        e.artifact(&self.schema);
        e.artifact(&self.profile);
        e.str(&self.build);
        e.str(&self.toolchain);
        e.u64(wide(self.rows.len()));
        for r in &self.rows {
            encode_row(&mut e, r);
        }
        encode_selection(&mut e, &self.selection);
        e.opt(self.publication.as_ref(), encode_publication);
        e.u64(wide(self.timings.len()));
        for t in &self.timings {
            e.str(&t.label);
            e.u64(t.measured_ns);
        }
        e.str(&self.grade);
        for list in [&self.tested_assumptions, &self.plan_assumptions] {
            e.u64(wide(list.len()));
            for a in list {
                e.str(a);
            }
        }
        let seal = Digest::of(BUNDLE_DOMAIN, &[e.as_bytes()]);
        e.digest(&seal);
        e.finish()
    }

    /// The parts `bytes` hold, unchecked: [`CandidateBundle::verify`] is what checks them.
    /// # Errors
    /// [`BundleRefusal::Magic`], [`BundleRefusal::Format`], [`BundleRefusal::Seal`] (checked before
    /// any part is read) or [`BundleRefusal::Wire`].
    pub fn decode(bytes: &[u8]) -> Result<UncheckedBundle, BundleRefusal> {
        let magic = bytes
            .get(..BUNDLE_MAGIC.len())
            .ok_or(BundleRefusal::Magic)?;
        if magic[..8] != BUNDLE_MAGIC[..8] {
            return Err(BundleRefusal::Magic);
        }
        if magic[8] != BUNDLE_MAGIC[8] {
            return Err(BundleRefusal::Format { found: magic[8] });
        }
        let body_len = bytes
            .len()
            .checked_sub(SEAL_LEN)
            .filter(|&n| n >= BUNDLE_MAGIC.len())
            .ok_or(BundleRefusal::Wire(WireFault::Truncated {
                field: "bundle.seal",
                need: SEAL_LEN,
                have: bytes.len() - BUNDLE_MAGIC.len(),
            }))?;
        let (body, closing) = bytes.split_at(body_len);
        let mut sealed = [0u8; SEAL_LEN];
        sealed.copy_from_slice(closing);
        let (sealed, found) = (Digest(sealed), Digest::of(BUNDLE_DOMAIN, &[body]));
        if sealed != found {
            return Err(BundleRefusal::Seal { sealed, found });
        }
        let mut d = Dec::new(&body[BUNDLE_MAGIC.len()..]);
        let parts = Self::decode_parts(&mut d).map_err(BundleRefusal::Wire)?;
        d.finish().map_err(BundleRefusal::Wire)?;
        Ok(parts)
    }

    fn decode_parts(d: &mut Dec<'_>) -> Result<UncheckedBundle, WireFault> {
        let policy = d.artifact("bundle.policy")?;
        let base = d.artifact("bundle.base")?;
        let candidate = d.artifact("bundle.candidate")?;
        let plan = d.bytes("bundle.plan")?.to_vec();
        let proposer = ProposerId {
            name: d.str("bundle.proposer")?.to_owned(),
            version: d.u32("bundle.proposer")?,
            config: d.digest("bundle.proposer")?,
        };
        let round = d.u8("bundle.round")?;
        let index = d.u64("bundle.index")?;
        let stranded_light = d.artifact("bundle.contract")?;
        let lane_fidelity = d.artifact("bundle.lane_fidelity")?;
        let interpreter = d.str("bundle.interpreter")?.to_owned();
        let lane = d.str("bundle.lane")?.to_owned();
        let oracle = d.str("bundle.oracle")?.to_owned();
        let corpus = d.digest("bundle.corpus")?;
        let schema = d.artifact("bundle.schema")?;
        let profile = d.artifact("bundle.profile")?;
        let build = d.str("bundle.build")?.to_owned();
        let toolchain = d.str("bundle.toolchain")?.to_owned();
        let n = d.count("bundle.rows", MAX_ROWS)?;
        let rows = (0..n)
            .map(|_| decode_row(d))
            .collect::<Result<Vec<_>, _>>()?;
        let selection = decode_selection(d)?;
        let publication = d.opt("bundle.publication", decode_publication)?;
        let n = d.count("bundle.timings", MAX_TIMINGS)?;
        let timings = (0..n)
            .map(|_| {
                Ok(Timing {
                    label: d.str("bundle.timing")?.to_owned(),
                    measured_ns: d.u64("bundle.timing")?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let grade = d.str("bundle.grade")?.to_owned();
        let tested_assumptions = decode_strs(d, "bundle.tested_assumptions")?;
        let plan_assumptions = decode_strs(d, "bundle.plan_assumptions")?;
        Ok(UncheckedBundle {
            policy,
            base,
            candidate,
            plan,
            proposer,
            round,
            index,
            stranded_light,
            lane_fidelity,
            interpreter,
            lane,
            oracle,
            corpus,
            schema,
            profile,
            build,
            toolchain,
            rows,
            selection,
            publication,
            timings,
            grade,
            tested_assumptions,
            plan_assumptions,
        })
    }
}

/// A bundle an evaluator built, or one [`Self::verify`] accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateBundle(UncheckedBundle);

impl CandidateBundle {
    /// The bundle of what an evaluator computed (`m4`).
    #[inline]
    #[must_use]
    pub(super) const fn computed(parts: UncheckedBundle) -> CandidateBundle {
        CandidateBundle(parts)
    }

    /// Its parts.
    #[inline]
    #[must_use]
    pub const fn parts(&self) -> &UncheckedBundle {
        &self.0
    }

    /// Bundle parts for declared/measured lab data and commit-witness publication.
    #[inline]
    pub(super) fn parts_mut(&mut self) -> &mut UncheckedBundle {
        &mut self.0
    }

    /// Its bytes, sealed.
    #[inline]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.0.encode()
    }

    /// The digest a status line and the lab name it by: `Digest::of(BUNDLE_DOMAIN, [its bytes])`.
    #[inline]
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(BUNDLE_DOMAIN, &[&self.encode()])
    }

    /// `bytes` decoded and checked against `evaluator`'s pinned policy.
    /// # Errors
    /// Any [`UncheckedBundle::decode`] refusal, then the first [`BundleRefusal::Forged`].
    pub fn verify(bytes: &[u8], evaluator: &Evaluator) -> Result<CandidateBundle, BundleRefusal> {
        let b = UncheckedBundle::decode(bytes)?;
        check_identities(&b, evaluator)?;
        let plan = SchedulePlan::decode(&b.plan).map_err(|_| forged(BundleField::Plan))?;
        if b.candidate != plan.artifact(&b.build) {
            return Err(forged(BundleField::Candidate));
        }
        check_proposer(&b)?;
        check_rows(&b, evaluator, plan)?;
        check_selection(&b, evaluator)?;
        check_grid(&b, evaluator, plan)?;
        if b.timings.iter().any(|t| t.label.is_empty()) {
            return Err(forged(BundleField::Timings));
        }
        if let Some(p) = &b.publication {
            check_publication(&b, p, evaluator)?;
        }
        Ok(CandidateBundle(b))
    }

    /// Selection provenance ([`SelectionProvenance::Rederived`] when credited to the grid).
    #[inline]
    #[must_use]
    pub fn selection_provenance(&self) -> SelectionProvenance {
        if credits_the_grid(&self.0) {
            SelectionProvenance::Rederived
        } else {
            SelectionProvenance::Declared
        }
    }
}

/// Provenance of a verified bundle's selection (mode, proposer, round, index, and counts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SelectionProvenance {
    /// Re-derived by re-running the policy's fixed grid proposals during verification.
    Rederived,
    /// Declared by the bundle sealer (unkeyed seal) and checked for consistency only.
    Declared,
}

impl SelectionProvenance {
    /// Its name in a summary line.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SelectionProvenance::Rederived => "rederived",
            SelectionProvenance::Declared => "declared",
        }
    }
}

fn check_identities(b: &UncheckedBundle, ev: &Evaluator) -> Result<(), BundleRefusal> {
    if b.policy != *ev.policy().id() {
        return Err(forged(BundleField::Policy));
    }
    if b.build != ev.build() {
        return Err(forged(BundleField::Build));
    }
    if b.base != MomentumCode::Shipped.artifact(&b.build) {
        return Err(forged(BundleField::Base));
    }
    let checks = [
        (
            b.stranded_light == NoStrandedLight::artifact(),
            BundleField::StrandedLightContract,
        ),
        (
            b.lane_fidelity == LaneFidelity::artifact(),
            BundleField::LaneFidelityContract,
        ),
        (b.interpreter == PLAN_INTERPRETER, BundleField::Interpreter),
        (b.lane == LANE_MODEL, BundleField::Lane),
        (b.oracle == ORACLE, BundleField::Oracle),
        (b.corpus == ev.corpus_digest(), BundleField::Corpus),
        (b.schema == schema_artifact(), BundleField::Schema),
        (b.profile == profile_artifact(), BundleField::Profile),
        (b.grade == BUNDLE_GRADE, BundleField::Grade),
        (
            b.tested_assumptions == TESTED_ASSUMPTIONS && b.plan_assumptions == PLAN_ASSUMPTIONS,
            BundleField::Assumptions,
        ),
    ];
    match checks.into_iter().find(|&(ok, _)| !ok) {
        Some((_, field)) => Err(forged(field)),
        None => Ok(()),
    }
}

fn check_proposer(b: &UncheckedBundle) -> Result<(), BundleRefusal> {
    let ok = match b.selection.mode {
        Mode::OnDemand => b.proposer == ProposerId::on_demand() && b.round == 0 && b.index == 0,
        Mode::Search => b.round < MAX_ROUNDS,
        Mode::Distributed => {
            b.round == 0
                && (b.proposer != GridProposer::new().id()
                    || usize::try_from(b.index)
                        .ok()
                        .and_then(|i| GridProposer::plans().get(i).cloned())
                        .is_some_and(|p| p == b.plan))
        }
    };
    if ok {
        Ok(())
    } else {
        Err(forged(BundleField::Proposer))
    }
}

fn credits_the_grid(b: &UncheckedBundle) -> bool {
    b.selection.mode == Mode::Search && b.proposer == GridProposer::new().id()
}

fn check_grid(
    b: &UncheckedBundle,
    ev: &Evaluator,
    plan: SchedulePlan,
) -> Result<(), BundleRefusal> {
    if !credits_the_grid(b) {
        return Ok(());
    }
    match ev.grid_selection() {
        Some(g) if g.plan == plan && g.round == b.round && g.index == b.index => {
            if g.stats == b.selection.stats {
                Ok(())
            } else {
                Err(forged(BundleField::Stats))
            }
        }
        _ => Err(forged(BundleField::Proposer)),
    }
}

fn check_rows(
    b: &UncheckedBundle,
    ev: &Evaluator,
    plan: SchedulePlan,
) -> Result<(), BundleRefusal> {
    let pins = ev.policy().items();
    for (k, pin) in pins.iter().enumerate() {
        match b.rows.get(k) {
            Some(r) if r.item == pin.id() && r.split == pin.split() && r.kind == pin.kind() => {}
            _ => return Err(forged_row(BundleField::Row, pin.id())),
        }
    }
    if let Some(extra) = b.rows.get(pins.len()) {
        return Err(forged_row(BundleField::Row, &extra.item));
    }
    for r in &b.rows {
        let field = if !r.run.passed() {
            Some(BundleField::Run)
        } else if !r.stranded_light.passed() {
            Some(BundleField::StrandedLightVerdict)
        } else if matches!(r.lane_fidelity, RowVerdict::Fail { .. }) {
            Some(BundleField::LaneFidelityVerdict)
        } else {
            None
        };
        if let Some(field) = field {
            return Err(forged_row(field, &r.item));
        }
    }
    for (k, r) in b.rows.iter().enumerate() {
        let Some(again) = ev.row_at(plan, k) else {
            return Err(forged_row(BundleField::Row, &r.item));
        };
        if let Some(field) = row_difference(r, &again) {
            return Err(forged_row(field, &r.item));
        }
    }
    Ok(())
}

/// The first part in which `r` is not the row `again` judged: its item, split or kind
/// ([`BundleField::Row`]), then its run, both verdicts, lane digest, chain, `T`, `P`, `Z` and
/// firings.
pub(super) fn row_difference(r: &Row, again: &Row) -> Option<BundleField> {
    if r.item != again.item || r.split != again.split || r.kind != again.kind {
        Some(BundleField::Row)
    } else if r.run != again.run {
        Some(BundleField::Run)
    } else if r.stranded_light != again.stranded_light {
        Some(BundleField::StrandedLightVerdict)
    } else if r.lane_fidelity != again.lane_fidelity {
        Some(BundleField::LaneFidelityVerdict)
    } else if r.lane != again.lane {
        Some(BundleField::LaneDigest)
    } else if r.chain != again.chain {
        Some(BundleField::Chain)
    } else if r.ticks != again.ticks {
        Some(BundleField::Ticks)
    } else if r.presented != again.presented {
        Some(BundleField::Presented)
    } else if r.wasted != again.wasted {
        Some(BundleField::Wasted)
    } else if r.firings != again.firings {
        Some(BundleField::Firings)
    } else {
        None
    }
}

/// Whether `r` is a passing row, as [`check_rows`] reads one: its run and `NoStrandedLight`
/// passed, and lane fidelity did not fail.
pub(super) fn row_passes(r: &Row) -> bool {
    r.run.passed()
        && r.stranded_light.passed()
        && !matches!(r.lane_fidelity, RowVerdict::Fail { .. })
}

fn check_selection(b: &UncheckedBundle, ev: &Evaluator) -> Result<(), BundleRefusal> {
    let s = &b.selection;
    let st = &s.stats;
    let counted = st
        .oversized
        .checked_add(st.duplicates)
        .and_then(|n| n.checked_add(st.over_budget))
        .and_then(|n| n.checked_add(st.distinct));
    let counts_ok = match s.mode {
        Mode::OnDemand => *st == SearchStats::ONE,
        Mode::Search | Mode::Distributed => {
            st.rounds == MAX_ROUNDS
                && counted == Some(st.proposed)
                && st.distinct <= wide(MAX_DISTINCT)
                && (1..=st.distinct).contains(&st.admitted)
                && (s.mode == Mode::Search
                    || b.proposer != GridProposer::new().id()
                    || grid_counts(st))
        }
    };
    if !counts_ok || s.held_out_looks != 1 || b.index >= st.proposed {
        return Err(forged(BundleField::Stats));
    }
    let Sums {
        train,
        held_out,
        s0: story,
    } = sums(ev.policy(), &b.rows);
    if s.train != train
        || s.held_out != held_out
        || s.s0 != story
        || (matches!(s.mode, Mode::Search | Mode::Distributed) && !train.beats())
    {
        return Err(forged(BundleField::Sums));
    }
    if s.useful != held_out.beats() {
        return Err(forged(BundleField::Useful));
    }
    if s.gain_ppm != held_out.gain_ppm() || s.s0_gain_ppm != story.gain_ppm() {
        return Err(forged(BundleField::Gain));
    }
    Ok(())
}

/// Whether `st` are the counts of the grid's one round: every plan proposed is distinct.
fn grid_counts(st: &SearchStats) -> bool {
    let plans = wide(GridProposer::plans().len());
    st.proposed == plans
        && st.distinct == plans
        && st.oversized == 0
        && st.duplicates == 0
        && st.over_budget == 0
}

/// `a + b`, field by field, saturating.
#[inline]
pub(super) const fn add(a: Totals, b: Totals) -> Totals {
    Totals {
        ticks: a.ticks.saturating_add(b.ticks),
        presented: a.presented.saturating_add(b.presented),
        wasted: a.wasted.saturating_add(b.wasted),
    }
}

fn check_publication(
    b: &UncheckedBundle,
    p: &Publication,
    ev: &Evaluator,
) -> Result<(), BundleRefusal> {
    let how = MomentumCode::ALL
        .iter()
        .find(|c| p.live_code == c.artifact(&b.build))
        .and_then(|&c| ev.policy().base(Sched::Prebuilt(c)));
    let by_rule = match how {
        Some(Replace::Optimization) => b.selection.useful,
        Some(Replace::Repair) => true,
        None => false,
    };
    let ok = p.candidate_code == b.candidate
        && p.live_code != b.candidate
        && by_rule
        && p.candidate_verdict.passed()
        && !matches!(p.live_verdict, RowVerdict::NotRun)
        && p.revision > p.head.revision
        && p.from <= p.to
        && p.to == p.head.input_cursor;
    if ok {
        Ok(())
    } else {
        Err(forged(BundleField::Publication))
    }
}
