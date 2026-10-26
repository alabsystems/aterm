// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Aos M5b's distributed search (design §11): the claims workers return, never evidence.
//!
//! # The locks
//!
//! A claim's rows cannot be read outside this module:
//!
//! ```compile_fail,E0616
//! use aterm_effects::aos_momentum::dist::Claim;
//! fn rows(c: &Claim) -> usize { c.rows.len() }
//! ```
//!
//! nor a claim given to the held-out gate:
//!
//! ```compile_fail,E0308
//! use aterm_effects::aos_momentum::dist::Claim;
//! use aterm_effects::aos_momentum::m4::Evaluator;
//! fn gate(ev: &Evaluator, c: Claim) { let _ = ev.held_out(c); }
//! ```

use core::fmt;
use core::ops::ControlFlow;
use std::collections::BTreeMap;

use aterm_aos::evidence::TESTED_ASSUMPTIONS;
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, Digest};

use super::bundle::{
    BUNDLE_GRADE, BundleField, PLAN_ASSUMPTIONS, Row, add, decode_row, encode_row, row_difference,
    row_passes,
};
use super::corpus::CorpusSplit;
use super::fidelity::{LaneFidelity, ORACLE};
use super::lane::LANE_MODEL;
use super::lease::{
    ClockBackwards, Effect, LeaseRefusal, Ledger, Limits, Nanos, Outcome, Token, VoidBy, WorkerId,
    WorkerState,
};
use super::m4::{DistNoOutcome, Evaluator, Job, TrainingEnd, TrainingRows};
use super::plan::{PLAN_INTERPRETER, PLAN_LEN, SchedulePlan};
use super::policy::{ItemPin, Totals};
use super::{MomentumCode, NoStrandedLight, profile_artifact, schema_artifact};

/// What a claim says of its candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimKind {
    /// Every training item passed.
    Admitted,
    /// The last row's item refused it.
    Refused,
}

impl ClaimKind {
    /// Its tag on the wire.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            ClaimKind::Admitted => 1,
            ClaimKind::Refused => 2,
        }
    }

    /// The kind a tag names.
    #[inline]
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<ClaimKind> {
        match tag {
            1 => Some(ClaimKind::Admitted),
            2 => Some(ClaimKind::Refused),
            _ => None,
        }
    }
}

/// Why a claim is not the shape of a training run (design §11.3, step 4): a filter, not evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeFault {
    /// An admitted claim without one row per training item, or a refused one with none or more.
    Count {
        /// What it claims.
        kind: ClaimKind,
        /// Its rows.
        rows: usize,
    },
    /// Row `row`'s item, split or kind is not training item `row`'s.
    Item {
        /// The row.
        row: usize,
    },
    /// Row `row` fails though the claim says admitted or a later row follows it.
    Fails {
        /// The row.
        row: usize,
    },
    /// A refused claim's last row passes: every refusal leaves a failing verdict in its row.
    LastPasses {
        /// The row.
        row: usize,
    },
    /// Row `row`'s `Z` is not `T − P` as the evaluator computes it.
    Wasted {
        /// The row.
        row: usize,
    },
}

impl fmt::Display for ShapeFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShapeFault::Count { kind, rows } => write!(
                f,
                "a claim {} with {rows} rows",
                match kind {
                    ClaimKind::Admitted => "admitted",
                    ClaimKind::Refused => "refused",
                }
            ),
            ShapeFault::Item { row } => {
                write!(f, "claimed row {row} is not training item {row}'s")
            }
            ShapeFault::Fails { row } => write!(
                f,
                "claimed row {row} fails, but the claim goes on or says admitted"
            ),
            ShapeFault::LastPasses { row } => write!(
                f,
                "the refused claim's last row {row} passes: a refusal leaves a failing verdict"
            ),
            ShapeFault::Wasted { row } => write!(f, "claimed row {row}'s Z is not T − P"),
        }
    }
}

impl std::error::Error for ShapeFault {}

/// A worker's claim of how one candidate fares on the training split (design §11.1): never
/// evidence. Its rows are private to this module, so a row a worker sent can be compared with a
/// row judged here ([`Self::check`]) but never put in a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    kind: ClaimKind,
    rows: Vec<Row>,
}

impl Claim {
    /// `rows` claimed `kind`, shaped against `pins`, the policy's items in corpus order.
    /// # Errors
    /// The first [`ShapeFault`].
    pub fn shaped(kind: ClaimKind, rows: Vec<Row>, pins: &[ItemPin]) -> Result<Claim, ShapeFault> {
        let mut train_len = 0usize;
        for p in pins {
            if p.split() == CorpusSplit::Train {
                train_len += 1;
            }
        }
        let fits = match kind {
            ClaimKind::Admitted => rows.len() == train_len,
            ClaimKind::Refused => !rows.is_empty() && rows.len() <= train_len,
        };
        if !fits {
            return Err(ShapeFault::Count {
                kind,
                rows: rows.len(),
            });
        }
        let last = rows.len().saturating_sub(1);
        let mut pin_idx = 0usize;
        for (row, r) in rows.iter().enumerate() {
            while pin_idx < pins.len() && pins[pin_idx].split() != CorpusSplit::Train {
                pin_idx += 1;
            }
            let pin = &pins[pin_idx];
            pin_idx += 1;
            if r.item != pin.id() || r.split != CorpusSplit::Train || r.kind != pin.kind() {
                return Err(ShapeFault::Item { row });
            }
            let refuses_here = kind == ClaimKind::Refused && row == last;
            match (row_passes(r), refuses_here) {
                (false, false) => return Err(ShapeFault::Fails { row }),
                (true, true) => return Err(ShapeFault::LastPasses { row }),
                _ => {}
            }
            if r.wasted != r.ticks.saturating_sub(r.presented) {
                return Err(ShapeFault::Wasted { row });
            }
        }
        Ok(Claim { kind, rows })
    }

    /// What it claims.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> ClaimKind {
        self.kind
    }

    /// Its rows' count.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether it has no rows (never, once shaped).
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Its rows' `ΣT`, `ΣP`, `ΣZ`, summed as the evaluator sums a candidate's.
    #[must_use]
    pub const fn totals(&self) -> Totals {
        let rows = self.rows.as_slice();
        let mut t = Totals {
            ticks: 0,
            presented: 0,
            wasted: 0,
        };
        let mut i = 0;
        while i < rows.len() {
            t = add(t, rows[i].totals());
            i += 1;
        }
        t
    }

    /// Row `k`'s claimed `T`.
    #[inline]
    #[must_use]
    pub const fn ticks(&self, k: usize) -> Option<u64> {
        let rows = self.rows.as_slice();
        if k < rows.len() {
            Some(rows[k].ticks)
        } else {
            None
        }
    }

    /// Whether row `k` is `judged`, a row judged here.
    /// # Errors
    /// The first part it differs in ([`BundleField::Row`] when there is no row `k`).
    pub fn check(&self, k: usize, judged: &Row) -> Result<(), BundleField> {
        let Some(r) = self.rows.get(k) else {
            return Err(BundleField::Row);
        };
        match row_difference(r, judged) {
            Some(field) => Err(field),
            None => Ok(()),
        }
    }

    /// Its encoding: the kind's tag, the row count, then each row as a bundle row.
    pub fn encode(&self, e: &mut Enc) {
        encode_claim(e, self.kind, &self.rows);
    }
}

fn encode_claim(e: &mut Enc, kind: ClaimKind, rows: &[Row]) {
    e.u8(kind.tag());
    e.u64(u64::try_from(rows.len()).unwrap_or(u64::MAX));
    for r in rows {
        encode_row(e, r);
    }
}

/// A claim as it came off the wire, not yet shaped: kept here, and shaped only once its lease
/// is accepted ([`Claim::shaped`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawClaim {
    kind: ClaimKind,
    rows: Vec<Row>,
}

impl RawClaim {
    /// Decoded from `d`: a count above [`MAX_ROWS`] is refused before any row is read.
    /// # Errors
    /// The first [`WireFault`].
    pub fn decode(d: &mut Dec<'_>) -> Result<RawClaim, WireFault> {
        let tag = d.u8("claim.kind")?;
        let kind = ClaimKind::from_tag(tag).ok_or(WireFault::BadTag {
            field: "claim.kind",
            tag,
        })?;
        let count = d.count("claim.rows", MAX_ROWS)?;
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            rows.push(decode_row(d)?);
        }
        Ok(RawClaim { kind, rows })
    }

    /// Its encoding, as [`Claim::encode`]'s.
    pub fn encode(&self, e: &mut Enc) {
        encode_claim(e, self.kind, &self.rows);
    }

    /// What it claims.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> ClaimKind {
        self.kind
    }

    /// Shaped against `pins`.
    /// # Errors
    /// The first [`ShapeFault`].
    pub fn shaped(self, pins: &[ItemPin]) -> Result<Claim, ShapeFault> {
        Claim::shaped(self.kind, self.rows, pins)
    }

    /// The claim with `f` applied to its rows (test faults only: a lying worker).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn altered(mut self, kind: ClaimKind, f: impl FnOnce(&mut Vec<Row>)) -> RawClaim {
        self.kind = kind;
        f(&mut self.rows);
        self
    }
}

impl From<Claim> for RawClaim {
    fn from(c: Claim) -> RawClaim {
        RawClaim {
            kind: c.kind,
            rows: c.rows,
        }
    }
}

/// The most rows a claim holds: one per training item of policy v1.
pub const MAX_ROWS: u64 = 17;

/// A job payload's magic.
pub const JOB_MAGIC: [u8; 7] = *b"AOSJOB\0";

/// The payload format this build speaks.
pub const JOB_FORMAT: u8 = 1;

/// The most bytes a frame's payload may hold, read from its header before the payload.
pub const MAX_FRAME: usize = 64 * 1024;

/// The domain of an execution identity's digest.
pub const EXEC_DOMAIN: &[u8] = b"aterm-aos-m5b-exec-v1\0";

/// The domain of a job id.
pub const JOB_DOMAIN: &[u8] = b"aterm-aos-m5b-job-v1\0";

/// The domain of a claim's digest.
pub const CLAIM_DOMAIN: &[u8] = b"aterm-aos-m5b-claim-v1\0";

/// The one job kind: a distinct candidate judged on the training split. Kinds 2 (proof) and 3
/// (replay) are reserved and refused.
pub const KIND_TRAIN: u8 = 1;

/// Whether this crate was compiled with debug assertions: part of a binary's build profile,
/// since the workspace compiles `aterm-effects` at its own opt-level.
pub const DEBUG_ASSERTIONS: bool = cfg!(debug_assertions);

/// The most faults a worker arms (test builds only).
const MAX_FAULTS: u64 = 16;

/// The most assumptions an identity lists.
const MAX_ASSUMPTIONS: u64 = 16;

/// What a worker must share with the coordinator for its claims to be judged at all (design
/// §11.2): what `check_identities` binds, the toolchain, the build profile, the executable and
/// the faults armed. A filter for mistakes, never evidence: a liar can name anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecIdentity {
    /// The acceptance policy.
    pub policy: ArtifactId,
    /// The corrected contract.
    pub stranded_light: ArtifactId,
    /// The lane-fidelity contract.
    pub lane_fidelity: ArtifactId,
    /// The bundle grade.
    pub grade: String,
    /// `TESTED_ASSUMPTIONS`.
    pub tested_assumptions: Vec<String>,
    /// `PLAN_ASSUMPTIONS`.
    pub plan_assumptions: Vec<String>,
    /// The corpus digest.
    pub corpus: Digest,
    /// The build the evaluator judges in.
    pub build: String,
    /// The toolchain's first `-vV` line.
    pub toolchain: String,
    /// The target and numerical profile.
    pub profile: ArtifactId,
    /// The cargo profile and opt-level the binary was built with.
    pub build_profile: String,
    /// The schema.
    pub schema: ArtifactId,
    /// The shipped base code.
    pub base: ArtifactId,
    /// The plan interpreter.
    pub interpreter: String,
    /// The lane model.
    pub lane: String,
    /// The oracle.
    pub oracle: String,
    /// The digest of the executable's own file.
    pub executable: Digest,
    /// The test faults armed, none in a measured run.
    pub faults: Vec<String>,
}

/// What the caller's binary contributes to an [`ExecIdentity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binary {
    /// The toolchain's first `-vV` line.
    pub toolchain: String,
    /// The cargo profile and opt-level.
    pub build_profile: String,
    /// The digest of the executable's own file.
    pub executable: Digest,
    /// The test faults armed.
    pub faults: Vec<String>,
}

impl ExecIdentity {
    /// The identity of `ev` in `binary`.
    #[must_use]
    pub fn of(ev: &Evaluator, binary: Binary) -> ExecIdentity {
        let build = ev.build().to_owned();
        let mut tested_assumptions = Vec::with_capacity(TESTED_ASSUMPTIONS.len());
        for a in TESTED_ASSUMPTIONS {
            tested_assumptions.push(a.to_owned());
        }
        let mut plan_assumptions = Vec::with_capacity(PLAN_ASSUMPTIONS.len());
        for a in PLAN_ASSUMPTIONS {
            plan_assumptions.push(a.to_owned());
        }
        ExecIdentity {
            policy: ev.policy().id().clone(),
            stranded_light: NoStrandedLight::artifact(),
            lane_fidelity: LaneFidelity::artifact(),
            grade: BUNDLE_GRADE.to_owned(),
            tested_assumptions,
            plan_assumptions,
            corpus: ev.corpus_digest(),
            base: MomentumCode::Shipped.artifact(&build),
            build,
            toolchain: binary.toolchain,
            profile: profile_artifact(),
            build_profile: binary.build_profile,
            schema: schema_artifact(),
            interpreter: PLAN_INTERPRETER.to_owned(),
            lane: LANE_MODEL.to_owned(),
            oracle: ORACLE.to_owned(),
            executable: binary.executable,
            faults: binary.faults,
        }
    }

    /// The first field, by name, in which `other` differs.
    #[must_use]
    pub fn first_difference(&self, other: &ExecIdentity) -> Option<&'static str> {
        let fields: [(&'static str, bool); 18] = [
            ("policy", self.policy == other.policy),
            (
                "stranded_light",
                self.stranded_light == other.stranded_light,
            ),
            ("lane_fidelity", self.lane_fidelity == other.lane_fidelity),
            ("grade", self.grade == other.grade),
            (
                "tested_assumptions",
                self.tested_assumptions == other.tested_assumptions,
            ),
            (
                "plan_assumptions",
                self.plan_assumptions == other.plan_assumptions,
            ),
            ("corpus", self.corpus == other.corpus),
            ("build", self.build == other.build),
            ("toolchain", self.toolchain == other.toolchain),
            ("profile", self.profile == other.profile),
            ("build_profile", self.build_profile == other.build_profile),
            ("schema", self.schema == other.schema),
            ("base", self.base == other.base),
            ("interpreter", self.interpreter == other.interpreter),
            ("lane", self.lane == other.lane),
            ("oracle", self.oracle == other.oracle),
            ("executable", self.executable == other.executable),
            ("faults", self.faults == other.faults),
        ];
        for (f, same) in fields {
            if !same {
                return Some(f);
            }
        }
        None
    }

    /// Its encoding.
    pub fn encode(&self, e: &mut Enc) {
        e.artifact(&self.policy);
        e.artifact(&self.stranded_light);
        e.artifact(&self.lane_fidelity);
        e.str(&self.grade);
        encode_strs(e, &self.tested_assumptions);
        encode_strs(e, &self.plan_assumptions);
        e.digest(&self.corpus);
        e.str(&self.build);
        e.str(&self.toolchain);
        e.artifact(&self.profile);
        e.str(&self.build_profile);
        e.artifact(&self.schema);
        e.artifact(&self.base);
        e.str(&self.interpreter);
        e.str(&self.lane);
        e.str(&self.oracle);
        e.digest(&self.executable);
        encode_strs(e, &self.faults);
    }

    /// Decoded from `d`.
    /// # Errors
    /// The first [`WireFault`].
    pub fn decode(d: &mut Dec<'_>) -> Result<ExecIdentity, WireFault> {
        Ok(ExecIdentity {
            policy: d.artifact("hello.policy")?,
            stranded_light: d.artifact("hello.stranded_light")?,
            lane_fidelity: d.artifact("hello.lane_fidelity")?,
            grade: d.str("hello.grade")?.to_owned(),
            tested_assumptions: decode_strs(d, "hello.tested_assumptions", MAX_ASSUMPTIONS)?,
            plan_assumptions: decode_strs(d, "hello.plan_assumptions", MAX_ASSUMPTIONS)?,
            corpus: d.digest("hello.corpus")?,
            build: d.str("hello.build")?.to_owned(),
            toolchain: d.str("hello.toolchain")?.to_owned(),
            profile: d.artifact("hello.profile")?,
            build_profile: d.str("hello.build_profile")?.to_owned(),
            schema: d.artifact("hello.schema")?,
            base: d.artifact("hello.base")?,
            interpreter: d.str("hello.interpreter")?.to_owned(),
            lane: d.str("hello.lane")?.to_owned(),
            oracle: d.str("hello.oracle")?.to_owned(),
            executable: d.digest("hello.executable")?,
            faults: decode_strs(d, "hello.faults", MAX_FAULTS)?,
        })
    }

    /// `Digest::of(EXEC_DOMAIN, [its encoding])`.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut e = Enc::new();
        self.encode(&mut e);
        Digest::of(EXEC_DOMAIN, &[e.as_bytes()])
    }
}

fn encode_strs(e: &mut Enc, v: &[String]) {
    e.u64(u64::try_from(v.len()).unwrap_or(u64::MAX));
    for s in v {
        e.str(s);
    }
}

fn decode_strs(d: &mut Dec<'_>, field: &'static str, limit: u64) -> Result<Vec<String>, WireFault> {
    let n = d.count(field, limit)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(d.str(field)?.to_owned());
    }
    Ok(out)
}

/// The id of the job that judges `plan` under the identity whose digest is `exec`.
#[must_use]
pub fn job_id(exec: &Digest, kind: u8, plan: SchedulePlan) -> Digest {
    Digest::of(JOB_DOMAIN, &[&exec.0, &[kind], &plan.encode()])
}

/// A claim's digest: its job id and the claim, without the worker's declared figures, so an
/// honest late result equals the accepted one.
#[must_use]
pub fn claim_digest(job: &Digest, claim: &RawClaim) -> Digest {
    let mut e = Enc::new();
    claim.encode(&mut e);
    Digest::of(CLAIM_DOMAIN, &[&job.0, e.as_bytes()])
}

fn encode_token(e: &mut Enc, t: &Token) {
    e.u32(t.search);
    e.u32(t.slot);
    e.u8(t.attempt);
    e.u32(t.worker);
}

fn decode_token(d: &mut Dec<'_>) -> Result<Token, WireFault> {
    Ok(Token {
        search: d.u32("token.search")?,
        slot: d.u32("token.slot")?,
        attempt: d.u8("token.attempt")?,
        worker: d.u32("token.worker")?,
    })
}

/// A message between the coordinator and a worker (design §11.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// Worker → coordinator, first: its identity, pid and the evaluator's build time (declared).
    Hello {
        /// Its identity (boxed: it is most of the message's size).
        identity: Box<ExecIdentity>,
        /// Its pid (declared).
        pid: u32,
        /// How long its evaluator took to build (declared).
        evaluator_ns: u64,
    },
    /// Coordinator → worker: judge `plan` under `token`.
    Lease {
        /// The lease.
        token: Token,
        /// The job's id.
        job: Digest,
        /// The job kind ([`KIND_TRAIN`]).
        kind: u8,
        /// The plan's bytes.
        plan: [u8; PLAN_LEN],
    },
    /// Coordinator → worker: stop `token`'s job.
    Cancel {
        /// The lease.
        token: Token,
    },
    /// Worker → coordinator: training item `k` of `token`'s job was just judged.
    Progress {
        /// The lease.
        token: Token,
        /// The item's training ordinal.
        k: u16,
    },
    /// Worker → coordinator: the claim for `token`'s job.
    Result {
        /// The lease.
        token: Token,
        /// The job's id.
        job: Digest,
        /// The claim.
        claim: RawClaim,
        /// Inputs stepped (declared).
        stepped: u64,
        /// Judging time (declared).
        judge_ns: u64,
    },
    /// Worker → coordinator: `token`'s job stopped after `at` items, as asked.
    Cancelled {
        /// The lease.
        token: Token,
        /// The items judged.
        at: u16,
    },
}

/// Why a payload is not a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameFault {
    /// The payload does not open with [`JOB_MAGIC`].
    Magic,
    /// It names another format.
    Format {
        /// The format found.
        found: u8,
    },
    /// No message has this tag.
    Tag {
        /// The tag.
        tag: u8,
    },
    /// The message travels the other way.
    Direction {
        /// The tag.
        tag: u8,
    },
    /// The body does not decode.
    Wire(WireFault),
    /// It decodes, but does not encode back to its bytes.
    NonCanonical,
    /// A lease of a job kind this build does not run.
    Kind {
        /// The kind.
        kind: u8,
    },
    /// The frame's header declares a payload over [`MAX_FRAME`] bytes.
    Oversize {
        /// The length declared.
        len: u64,
    },
    /// The frame's header or checksum does not hold, or the stream ended inside a frame.
    Corrupt {
        /// What the reader found.
        detail: String,
    },
}

impl From<WireFault> for FrameFault {
    fn from(w: WireFault) -> FrameFault {
        FrameFault::Wire(w)
    }
}

impl fmt::Display for FrameFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameFault::Magic => f.write_str("the payload is not a job message"),
            FrameFault::Format { found } => write!(
                f,
                "the payload is format {found}; this build speaks format {JOB_FORMAT}"
            ),
            FrameFault::Tag { tag } => write!(f, "no message has tag {tag}"),
            FrameFault::Direction { tag } => {
                write!(f, "message tag {tag} travels the other way")
            }
            FrameFault::Wire(w) => write!(f, "the message does not decode: {w}"),
            FrameFault::NonCanonical => {
                f.write_str("the message does not encode back to its bytes")
            }
            FrameFault::Kind { kind } => write!(f, "job kind {kind} is not run by this build"),
            FrameFault::Oversize { len } => write!(
                f,
                "the frame declares {len} payload bytes, over the {MAX_FRAME}-byte bound"
            ),
            FrameFault::Corrupt { detail } => write!(f, "the frame is corrupt: {detail}"),
        }
    }
}

impl std::error::Error for FrameFault {}

/// Who sends a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sender {
    /// A worker.
    Worker,
    /// The coordinator.
    Coordinator,
}

impl Msg {
    /// Its tag.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self {
            Msg::Hello { .. } => 1,
            Msg::Lease { .. } => 2,
            Msg::Cancel { .. } => 3,
            Msg::Progress { .. } => 4,
            Msg::Result { .. } => 5,
            Msg::Cancelled { .. } => 6,
        }
    }

    /// Who sends a message with `tag`.
    const fn sender(tag: u8) -> Option<Sender> {
        match tag {
            1 | 4 | 5 | 6 => Some(Sender::Worker),
            2 | 3 => Some(Sender::Coordinator),
            _ => None,
        }
    }

    /// Its payload: the magic, the format, the tag, then the body.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.raw(&JOB_MAGIC);
        e.u8(JOB_FORMAT);
        e.u8(self.tag());
        match self {
            Msg::Hello {
                identity,
                pid,
                evaluator_ns,
            } => {
                identity.encode(&mut e);
                e.u32(*pid);
                e.u64(*evaluator_ns);
            }
            Msg::Lease {
                token,
                job,
                kind,
                plan,
            } => {
                encode_token(&mut e, token);
                e.digest(job);
                e.u8(*kind);
                e.raw(plan);
            }
            Msg::Cancel { token } => encode_token(&mut e, token),
            Msg::Progress { token, k } => {
                encode_token(&mut e, token);
                e.u16(*k);
            }
            Msg::Result {
                token,
                job,
                claim,
                stepped,
                judge_ns,
            } => {
                encode_token(&mut e, token);
                e.digest(job);
                claim.encode(&mut e);
                e.u64(*stepped);
                e.u64(*judge_ns);
            }
            Msg::Cancelled { token, at } => {
                encode_token(&mut e, token);
                e.u16(*at);
            }
        }
        e.finish()
    }

    /// The message `bytes` hold, sent by `from`: every count bounded, nothing trailing, and
    /// encoding back to `bytes`.
    /// # Errors
    /// The first [`FrameFault`].
    pub fn decode(bytes: &[u8], from: Sender) -> Result<Msg, FrameFault> {
        let mut d = Dec::new(bytes);
        if d.raw("job.magic", JOB_MAGIC.len()).ok() != Some(&JOB_MAGIC[..]) {
            return Err(FrameFault::Magic);
        }
        let format = d.u8("job.format")?;
        if format != JOB_FORMAT {
            return Err(FrameFault::Format { found: format });
        }
        let tag = d.u8("job.tag")?;
        match Msg::sender(tag) {
            None => return Err(FrameFault::Tag { tag }),
            Some(s) if s != from => return Err(FrameFault::Direction { tag }),
            Some(_) => {}
        }
        let msg = match tag {
            1 => Msg::Hello {
                identity: Box::new(ExecIdentity::decode(&mut d)?),
                pid: d.u32("hello.pid")?,
                evaluator_ns: d.u64("hello.evaluator_ns")?,
            },
            2 => {
                let token = decode_token(&mut d)?;
                let job = d.digest("lease.job")?;
                let kind = d.u8("lease.kind")?;
                if kind != KIND_TRAIN {
                    return Err(FrameFault::Kind { kind });
                }
                let mut plan = [0u8; PLAN_LEN];
                plan.copy_from_slice(d.raw("lease.plan", PLAN_LEN)?);
                Msg::Lease {
                    token,
                    job,
                    kind,
                    plan,
                }
            }
            3 => Msg::Cancel {
                token: decode_token(&mut d)?,
            },
            4 => Msg::Progress {
                token: decode_token(&mut d)?,
                k: d.u16("progress.k")?,
            },
            5 => Msg::Result {
                token: decode_token(&mut d)?,
                job: d.digest("result.job")?,
                claim: RawClaim::decode(&mut d)?,
                stepped: d.u64("result.stepped")?,
                judge_ns: d.u64("result.judge_ns")?,
            },
            _ => Msg::Cancelled {
                token: decode_token(&mut d)?,
                at: d.u16("cancelled.at")?,
            },
        };
        d.finish()?;
        if msg.encode() != bytes {
            return Err(FrameFault::NonCanonical);
        }
        Ok(msg)
    }
}

/// How a job ended in a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobEnd {
    /// It ran to its claim.
    Claimed(Claim),
    /// `keep` stopped it after `at` items.
    Stopped {
        /// The items judged.
        at: u16,
    },
}

/// A worker's job: `plan` judged on the training split by `ev`, item by item, `keep` told each
/// passing item's ordinal and able to stop the run. Reads no held-out item.
/// # Errors
/// The [`ShapeFault`] of a run whose rows are not a training run's (never, for this evaluator).
pub fn run_job(
    ev: &Evaluator,
    plan: SchedulePlan,
    keep: &mut dyn FnMut(usize) -> ControlFlow<()>,
) -> Result<JobEnd, ShapeFault> {
    let TrainingRows { rows, end } = ev.training_rows(plan, keep);
    let at = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    let kind = match end {
        TrainingEnd::Admitted => ClaimKind::Admitted,
        TrainingEnd::Refused(_) => ClaimKind::Refused,
        TrainingEnd::Stopped(()) => return Ok(JobEnd::Stopped { at }),
    };
    let mut plain_rows = Vec::with_capacity(rows.len());
    for (_, r) in rows {
        plain_rows.push(r);
    }
    Claim::shaped(kind, plain_rows, ev.policy().items()).map(JobEnd::Claimed)
}

/// Why the coordinator voided the command's distributed work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A worker's payload is not a message.
    Frame {
        /// The sender.
        worker: WorkerId,
        /// Why.
        fault: FrameFault,
    },
    /// The ledger refused a frame, or ran out of attempts or workers.
    Lease(LeaseRefusal),
    /// An accepted claim is not a training run's shape.
    Shape {
        /// The sender.
        worker: WorkerId,
        /// The job.
        slot: u32,
        /// Why.
        fault: ShapeFault,
    },
    /// A result names another job than its lease's.
    JobId {
        /// The sender.
        worker: WorkerId,
        /// The token sent.
        sent: Token,
    },
    /// The distributed search has no result.
    Dist(DistNoOutcome),
    /// The command was interrupted.
    Interrupted,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Frame { worker, fault } => write!(f, "worker {}'s frame: {fault}", worker.0),
            Refusal::Lease(r) => r.fmt(f),
            Refusal::Shape {
                worker,
                slot,
                fault,
            } => write!(f, "worker {}'s claim for job {slot}: {fault}", worker.0),
            Refusal::JobId { worker, sent } => write!(
                f,
                "worker {}'s result for job {} names another job id",
                worker.0, sent.slot
            ),
            Refusal::Dist(no) => no.fmt(f),
            Refusal::Interrupted => f.write_str("interrupted"),
        }
    }
}

impl std::error::Error for Refusal {}

impl Refusal {
    /// Its name in a summary line.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Refusal::Frame { .. } => "Frame",
            Refusal::Lease(r) => match r {
                LeaseRefusal::NotJoined { .. } => "NotJoined",
                LeaseRefusal::SecondHello { .. } => "SecondHello",
                LeaseRefusal::NotHeld { .. } => "NotHeld",
                LeaseRefusal::NoProgress { .. } => "NoProgress",
                LeaseRefusal::Unasked { .. } => "Unasked",
                LeaseRefusal::Differs { .. } => "Differs",
                LeaseRefusal::Exhausted { .. } => "Exhausted",
                LeaseRefusal::NoWorkers { .. } => "NoWorkers",
            },
            Refusal::Shape { .. } => "Shape",
            Refusal::JobId { .. } => "NotHeld",
            Refusal::Dist(no) => match no {
                DistNoOutcome::Contradicted { .. } => "Contradicted",
                DistNoOutcome::Adaptive { .. } => "Adaptive",
                DistNoOutcome::Unclaimed { .. } => "Unclaimed",
                DistNoOutcome::Misplaced { .. } => "Misplaced",
                DistNoOutcome::Local(_) => "Local",
            },
            Refusal::Interrupted => "Cancelled",
        }
    }
}

/// An event from the shell about one worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A frame's payload.
    Frame(Vec<u8>),
    /// Its pipe closed or it exited.
    Eof,
    /// Its reader found a frame it could not read (header, length or CRC) and stopped.
    Broken(FrameFault),
}

/// What the shell must do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send this payload to the worker.
    Send(WorkerId, Vec<u8>),
    /// Kill the worker and stop reading it.
    Kill(WorkerId),
}

/// What a worker declared in its hello (never scored).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Declared {
    /// Its pid.
    pub pid: u32,
    /// How long its evaluator took to build.
    pub evaluator_ns: u64,
}

/// One accepted job's round trip, from the `now`s the shell passed (the core reads no clock) and
/// the worker's declared figures. Measured, never scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trip {
    /// The lease.
    pub token: Token,
    /// The claim's kind.
    pub kind: ClaimKind,
    /// When the lease was issued.
    pub leased: Nanos,
    /// When its result was accepted.
    pub accepted: Nanos,
    /// The worker's judging time (declared).
    pub judge_ns: u64,
    /// The inputs it stepped (declared).
    pub stepped: u64,
}

/// The coordinator's core (design §11.4): pure, fed frames and the clock by the shell.
#[derive(Debug)]
pub struct Coordinator {
    ledger: Ledger,
    identity: ExecIdentity,
    exec: Digest,
    pins: Vec<ItemPin>,
    /// The current search's jobs and their ids.
    jobs: Vec<(Job, Digest)>,
    /// The current search's accepted claims.
    claims: Vec<Option<Claim>>,
    refusal: Option<Refusal>,
    /// Each worker's hello, once given.
    declared: Vec<Option<Declared>>,
    /// Each foreign worker and the first field it differs in.
    foreign: Vec<(WorkerId, &'static str)>,
    /// When each live lease was issued.
    leased: BTreeMap<Token, Nanos>,
    /// Every accepted job's round trip, in acceptance order.
    trips: Vec<Trip>,
}

impl Coordinator {
    /// A coordinator of `identity` under `limits`, judging with `pins`, the policy's items.
    #[must_use]
    pub fn new(identity: ExecIdentity, limits: Limits, pins: &[ItemPin]) -> Coordinator {
        Coordinator {
            ledger: Ledger::new(limits),
            exec: identity.digest(),
            identity,
            pins: pins.to_vec(),
            jobs: Vec::new(),
            claims: Vec::new(),
            refusal: None,
            declared: Vec::new(),
            foreign: Vec::new(),
            leased: BTreeMap::new(),
            trips: Vec::new(),
        }
    }

    /// The ledger.
    #[inline]
    #[must_use]
    pub const fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Its identity's digest.
    #[inline]
    #[must_use]
    pub const fn exec(&self) -> Digest {
        self.exec
    }

    /// Why the work is void, if it is.
    #[inline]
    #[must_use]
    pub const fn refusal(&self) -> Option<&Refusal> {
        self.refusal.as_ref()
    }

    /// Each worker's hello, by id.
    #[inline]
    #[must_use]
    pub const fn declared(&self) -> &[Option<Declared>] {
        self.declared.as_slice()
    }

    /// The workers refused for a foreign hello.
    #[inline]
    #[must_use]
    pub const fn foreign(&self) -> &[(WorkerId, &'static str)] {
        self.foreign.as_slice()
    }

    /// Every accepted job's round trip, in acceptance order, across the command's searches.
    #[inline]
    #[must_use]
    pub const fn trips(&self) -> &[Trip] {
        self.trips.as_slice()
    }

    /// A new worker, or `None` past the limit.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn join(&mut self, now: Nanos) -> Result<Option<WorkerId>, ClockBackwards> {
        let w = self.ledger.join(now)?;
        if w.is_some() {
            self.declared.push(None);
        }
        Ok(w)
    }

    /// Opens the next search on `jobs` and leases the idle workers.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn begin(&mut self, jobs: &[Job], now: Nanos) -> Result<Vec<Action>, ClockBackwards> {
        let search = self.ledger.search().saturating_add(1);
        let n = u32::try_from(jobs.len()).unwrap_or(u32::MAX);
        let mut effects = self.ledger.begin(search, n, now)?;
        if self.ledger.search() == search {
            let mut pairs = Vec::with_capacity(jobs.len());
            for &j in jobs {
                pairs.push((j, job_id(&self.exec, KIND_TRAIN, j.plan)));
            }
            self.jobs = pairs;
            self.claims = vec![None; jobs.len()];
        }
        effects.extend(self.ledger.assign(now)?);
        self.ledger.finish(now)?;
        Ok(self.actions(effects, now))
    }

    /// The current search's accepted claims, by slot, once it is complete.
    #[must_use]
    pub fn take_claims(&mut self) -> Vec<(u32, Claim)> {
        if self.ledger.outcome() != Outcome::Complete {
            return Vec::new();
        }
        let taken = core::mem::take(&mut self.claims);
        let mut out = Vec::with_capacity(taken.len());
        for (c, (j, _)) in taken.into_iter().zip(&self.jobs) {
            if let Some(c) = c {
                out.push((j.slot, c));
            }
        }
        out
    }

    /// `w`'s event.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn on(
        &mut self,
        w: WorkerId,
        event: Event,
        now: Nanos,
    ) -> Result<Vec<Action>, ClockBackwards> {
        let mut effects = match event {
            Event::Eof => self.ledger.lost(w, now)?,
            Event::Frame(bytes) => self.frame(w, &bytes, now)?,
            Event::Broken(fault) => self.broken(w, fault, now)?,
        };
        effects.extend(self.ledger.assign(now)?);
        self.ledger.finish(now)?;
        Ok(self.actions(effects, now))
    }

    /// A transport fault voids as its sender's frame fault, unless the sender is already gone: a
    /// worker this coordinator killed can leave a cut frame in its pipe, and nothing it sent after
    /// it was gone is read.
    fn broken(
        &mut self,
        w: WorkerId,
        fault: FrameFault,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        if matches!(self.ledger.worker(w), Some(WorkerState::Gone(_)) | None) {
            self.ledger.drop_unread(now)?;
            return Ok(Vec::new());
        }
        self.void_from(w, Refusal::Frame { worker: w, fault }, now)
    }

    fn frame(
        &mut self,
        w: WorkerId,
        bytes: &[u8],
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        if matches!(self.ledger.worker(w), Some(WorkerState::Gone(_)) | None) {
            self.ledger.drop_unread(now)?;
            return Ok(Vec::new());
        }
        let msg = match Msg::decode(bytes, Sender::Worker) {
            Ok(m) => m,
            Err(fault) => return self.void_from(w, Refusal::Frame { worker: w, fault }, now),
        };
        match msg {
            Msg::Hello {
                identity,
                pid,
                evaluator_ns,
            } => {
                let matched = match self.identity.first_difference(&identity) {
                    None => Ok(()),
                    Some(field) => Err(field),
                };
                if matches!(self.ledger.worker(w), Some(WorkerState::Joining { .. })) {
                    if let Some(at) = self.declared.get_mut(index(w.0)) {
                        *at = Some(Declared { pid, evaluator_ns });
                    }
                    if let Err(field) = matched {
                        self.foreign.push((w, field));
                    }
                }
                self.ledger.hello(w, matched, now)
            }
            Msg::Progress { token, k } => self.ledger.progress(w, token, k, now),
            Msg::Cancelled { token, .. } => self.ledger.cancelled(w, token, now),
            Msg::Result {
                token,
                job,
                claim,
                stepped,
                judge_ns,
            } => self.result(w, token, job, claim, (stepped, judge_ns), now),
            Msg::Lease { .. } | Msg::Cancel { .. } => {
                let fault = FrameFault::Direction { tag: msg.tag() };
                self.void_from(w, Refusal::Frame { worker: w, fault }, now)
            }
        }
    }

    fn result(
        &mut self,
        w: WorkerId,
        token: Token,
        job: Digest,
        claim: RawClaim,
        (stepped, judge_ns): (u64, u64),
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        let live = matches!(
            self.ledger.worker(w),
            Some(WorkerState::Busy { lease, .. }) if lease == token
        );
        if live && self.jobs.get(index(token.slot)).map(|(_, id)| *id) != Some(job) {
            return self.void_from(
                w,
                Refusal::JobId {
                    worker: w,
                    sent: token,
                },
                now,
            );
        }
        let digest = claim_digest(&job, &claim);
        let effects = self.ledger.result(w, token, digest, now)?;
        if effects.contains(&Effect::Accepted(w, token)) {
            match claim.shaped(&self.pins) {
                Ok(c) => {
                    if let Some(leased) = self.leased.remove(&token) {
                        self.trips.push(Trip {
                            token,
                            kind: c.kind(),
                            leased,
                            accepted: now,
                            judge_ns,
                            stepped,
                        });
                    }
                    if let Some(at) = self.claims.get_mut(index(token.slot)) {
                        *at = Some(c);
                    }
                }
                Err(fault) => {
                    let mut out = effects;
                    let why = Refusal::Shape {
                        worker: w,
                        slot: token.slot,
                        fault,
                    };
                    out.extend(self.void_from(w, why, now)?);
                    return Ok(out);
                }
            }
        }
        self.note_ledger_void();
        Ok(effects)
    }

    /// Voids for `why`, found in `w`'s frame.
    fn void_from(
        &mut self,
        w: WorkerId,
        why: Refusal,
        now: Nanos,
    ) -> Result<Vec<Effect>, ClockBackwards> {
        let effects = self.ledger.void_from(w, VoidBy::Caller, now)?;
        if self.refusal.is_none() {
            self.refusal = Some(why);
        }
        Ok(effects)
    }

    /// Records the ledger's own refusal, once it voided.
    fn note_ledger_void(&mut self) {
        if self.refusal.is_none()
            && let Outcome::Void(VoidBy::Ledger(r)) = self.ledger.outcome()
        {
            self.refusal = Some(Refusal::Lease(r));
        }
    }

    /// Expires what is due at `now`.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn tick(&mut self, now: Nanos) -> Result<Vec<Action>, ClockBackwards> {
        let mut effects = self.ledger.tick(now)?;
        effects.extend(self.ledger.assign(now)?);
        self.ledger.finish(now)?;
        Ok(self.actions(effects, now))
    }

    /// Voids for `why`, found after the search (step 5) or by an interrupt.
    /// # Errors
    /// [`ClockBackwards`].
    pub fn void(&mut self, why: Refusal, now: Nanos) -> Result<Vec<Action>, ClockBackwards> {
        let by = if why == Refusal::Interrupted {
            VoidBy::Interrupted
        } else {
            VoidBy::Caller
        };
        let effects = self.ledger.void(by, now)?;
        if self.refusal.is_none() {
            self.refusal = Some(why);
        }
        Ok(self.actions(effects, now))
    }

    /// The ledger's effects as the shell's actions.
    fn actions(&mut self, effects: Vec<Effect>, now: Nanos) -> Vec<Action> {
        self.note_ledger_void();
        let mut out = Vec::with_capacity(effects.len());
        for e in effects {
            match e {
                Effect::Lease(w, token) => {
                    if let Some(&(j, id)) = self.jobs.get(index(token.slot)) {
                        self.leased.insert(token, now);
                        let msg = Msg::Lease {
                            token,
                            job: id,
                            kind: KIND_TRAIN,
                            plan: j.plan.encode(),
                        };
                        out.push(Action::Send(w, msg.encode()));
                    }
                }
                Effect::Cancel(w, token) => {
                    out.push(Action::Send(w, Msg::Cancel { token }.encode()));
                }
                Effect::Kill(w) => out.push(Action::Kill(w)),
                Effect::Accepted(..) => {}
            }
        }
        out
    }
}

#[inline]
const fn index(n: u32) -> usize {
    n as usize
}
