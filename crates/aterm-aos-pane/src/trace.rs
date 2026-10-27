// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A recorded pane chain as one sealed file, replayed under `pane.faithful-replay` in this process or another.

use core::fmt;

use aterm_aos::checkpoint::{Checkpoint, CheckpointFault, Provenance, RestoreRefusal};
use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::{EffectRecord, EffectRequest, ShadowPort};
use aterm_aos::log::{EventLog, IdentityMismatch, LogFault, LogFull, LogHeader, Tail};
use aterm_aos::replay::{Run, replay};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{BranchId, Digest, Revision, TaskIdentity};
use aterm_core::terminal::{ClockReading, HostOp};

use crate::{
    FaithfulReplay, PaneClock, PaneCode, PaneCounts, PaneInput, PaneSchema, PaneState,
    PrebuiltPane, REPLIES_MATCH, REPLY_TARGET, REPLY_WRITE, pane_identity,
};

/// The bytes that open a pane trace file.
pub const PANE_TRACE_MAGIC: [u8; 8] = *b"AOSPTR\0\x01";

/// The domain of the digest that closes a pane trace file.
pub const PANE_TRACE_DOMAIN: &[u8] = b"aterm-aos-pane-trace-file-v1\0";

/// The most inputs one pane trace holds.
pub const PANE_MAX_EVENTS: usize = 1 << 20;

/// The most bytes a reader of pane traces reads before it decodes one: past twice the largest chain a Rewind recorder exports (a checkpoint at the 16 MiB frame cap and its 8 MiB budget of output).
pub const PANE_TRACE_MAX_BYTES: usize = 64 << 20;

const SEAL_LEN: usize = 32;

fn len_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// One recorded chain of a pane: where it starts, what followed, and the replies the live engine made, as a host builds it.
#[derive(Clone, Debug)]
pub struct PaneRecording {
    schema: PaneSchema,
    start: PaneState,
    inputs: Vec<PaneInput>,
    effects: Vec<EffectRecord>,
}

/// What a replay of a recording reached and the contract's verdict on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneCheck {
    /// The code that ran.
    pub code: PaneCode,
    /// The start state's digest.
    pub start: Digest,
    /// The state the replay reached.
    pub state_digest: Digest,
    /// Its observation chain.
    pub observation_chain: Digest,
    /// The inputs by kind and the witnesses matched.
    pub counts: PaneCounts,
    /// Replies that met their recorded effects.
    pub replies_matched: u64,
    /// The verdict.
    pub verdict: Verdict,
}

/// Why a pane trace was not built, read or replayed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneTraceFault {
    /// The bytes do not open with `AOSPTR\0`.
    Magic,
    /// They open with another format byte than [`PANE_TRACE_MAGIC`]'s.
    Format {
        /// The byte found.
        found: u8,
    },
    /// The closing digest does not name the bytes before it.
    Seal {
        /// The digest the file closes with.
        sealed: Digest,
        /// The digest of its bytes.
        found: Digest,
    },
    /// A field does not decode, or bytes follow the end.
    Wire(WireFault),
    /// The identity is none this build runs a pane under: the first artifact that differs.
    Identity(IdentityMismatch),
    /// The checkpoint does not decode.
    Checkpoint(CheckpointFault),
    /// It does not restore under the identity.
    Restore(RestoreRefusal),
    /// The log does not decode beside the checkpoint.
    Log(LogFault),
    /// The log is torn or unsealed.
    Tail(Tail),
    /// More inputs than one trace holds.
    Full(LogFull),
    /// The replay reached another state than the exporter's.
    State {
        /// The exporter's.
        recorded: Digest,
        /// This replay's.
        replayed: Digest,
    },
    /// The replay observed another chain than the exporter's.
    Chain {
        /// The exporter's.
        recorded: Digest,
        /// This replay's.
        replayed: Digest,
    },
}

impl PaneTraceFault {
    /// The fault as one word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            PaneTraceFault::Magic => "magic",
            PaneTraceFault::Format { .. } => "format",
            PaneTraceFault::Seal { .. } => "seal",
            PaneTraceFault::Wire(_) => "wire",
            PaneTraceFault::Identity(_) => "identity",
            PaneTraceFault::Checkpoint(_) => "checkpoint",
            PaneTraceFault::Restore(_) => "restore",
            PaneTraceFault::Log(_) => "log",
            PaneTraceFault::Tail(_) => "tail",
            PaneTraceFault::Full(_) => "full",
            PaneTraceFault::State { .. } => "state",
            PaneTraceFault::Chain { .. } => "chain",
        }
    }
}

impl fmt::Display for PaneTraceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaneTraceFault::Magic => f.write_str("the bytes are not a pane trace file"),
            PaneTraceFault::Format { found } => write!(
                f,
                "the pane trace is format {found}; this build reads format {}",
                PANE_TRACE_MAGIC[7]
            ),
            PaneTraceFault::Seal { sealed, found } => write!(
                f,
                "the trace closes with the digest {}, but its bytes digest to {}: it was cut or altered",
                sealed.short(),
                found.short()
            ),
            PaneTraceFault::Wire(w) => write!(f, "the trace does not decode: {w}"),
            PaneTraceFault::Identity(m) => write!(
                f,
                "the trace was recorded under an identity this build does not run: {m}"
            ),
            PaneTraceFault::Checkpoint(c) => write!(f, "the checkpoint does not decode: {c}"),
            PaneTraceFault::Restore(r) => write!(f, "the checkpoint does not restore: {r}"),
            PaneTraceFault::Log(l) => write!(f, "the log does not decode: {l}"),
            PaneTraceFault::Tail(t) => write!(f, "the log is not whole: {t:?}"),
            PaneTraceFault::Full(l) => write!(f, "{l}"),
            PaneTraceFault::State { recorded, replayed } => write!(
                f,
                "the replay reached the state {}, not the exporter's {}",
                replayed.short(),
                recorded.short()
            ),
            PaneTraceFault::Chain { recorded, replayed } => write!(
                f,
                "the replay observed the chain {}, not the exporter's {}",
                replayed.short(),
                recorded.short()
            ),
        }
    }
}

impl std::error::Error for PaneTraceFault {}

fn effect_record(seq: u64, reply: &[u8]) -> EffectRecord {
    EffectRecord {
        seq,
        target: REPLY_TARGET,
        op: REPLY_WRITE,
        payload: EffectRequest {
            target: REPLY_TARGET,
            op: REPLY_WRITE,
            payload: reply.to_vec(),
        }
        .payload_digest(),
        result: Vec::new(),
    }
}

/// Replays `inputs` from `start` under `code`, its replies shadowed by `effects`, and judges it.
fn run(
    schema: &PaneSchema,
    code: &PrebuiltPane,
    start: PaneState,
    inputs: &[PaneInput],
    effects: &[EffectRecord],
) -> (Run<PaneSchema>, PaneCheck) {
    let start_digest = aterm_aos::task::state_digest(schema, &start);
    let mut port = ShadowPort::new(effects);
    let run = replay(schema, code, start, inputs, 0, &mut port);
    let replies_matched = port.matched();
    let mut verdict = FaithfulReplay::new().judge(schema, inputs, &run);
    if verdict.passed()
        && let Err(d) = port.finish()
    {
        verdict = Verdict::Fail {
            cursor: len_u64(inputs.len()),
            law: REPLIES_MATCH,
            detail: d.to_string(),
        };
    }
    let check = PaneCheck {
        code: code.code,
        start: start_digest,
        state_digest: run.state_digest,
        observation_chain: run.observation_chain,
        counts: PaneCounts::of(inputs, &run),
        replies_matched,
        verdict,
    };
    (run, check)
}

impl PaneRecording {
    /// A chain starting at `start`, its clocks offsets from `origin`.
    #[must_use]
    pub fn new(origin: ClockReading, start: PaneState) -> Self {
        PaneRecording {
            schema: PaneSchema::new(origin),
            start,
            inputs: Vec::new(),
            effects: Vec::new(),
        }
    }

    /// The schema the chain's clocks count on.
    #[must_use]
    pub const fn schema(&self) -> &PaneSchema {
        &self.schema
    }

    /// The inputs so far.
    #[must_use]
    pub fn inputs(&self) -> &[PaneInput] {
        &self.inputs
    }

    /// The recorded replies so far, one a slice.
    #[must_use]
    pub fn effects(&self) -> &[EffectRecord] {
        &self.effects
    }

    /// A slice the live engine parsed at `clock` and the replies it made (empty when none).
    pub fn bytes(&mut self, bytes: std::sync::Arc<[u8]>, clock: Option<PaneClock>, reply: &[u8]) {
        let seq = len_u64(self.effects.len());
        self.effects.push(effect_record(seq, reply));
        self.inputs.push(PaneInput::Bytes { bytes, clock });
    }

    /// A resize the live engine applied.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.inputs.push(PaneInput::Resize { rows, cols });
    }

    /// A host cell-pixel box `(width, height)` the live engine applied.
    pub fn cell_px(&mut self, width: u16, height: u16) {
        self.inputs.push(PaneInput::CellPx { width, height });
    }

    /// A host-side mutation the live engine applied, with its clock when time-sensitive.
    pub fn host(&mut self, op: HostOp, clock: Option<PaneClock>) {
        self.inputs.push(PaneInput::Host { op, clock });
    }

    /// The live screen here ([`crate::screen_digest`]).
    pub fn witness(&mut self, screen: Digest) {
        self.inputs.push(PaneInput::Witness { screen });
    }

    /// The inputs and recorded replies, for a test to alter.
    #[cfg(test)]
    pub(crate) const fn parts_mut(&mut self) -> (&mut Vec<PaneInput>, &mut Vec<EffectRecord>) {
        (&mut self.inputs, &mut self.effects)
    }

    /// Replays the chain under `code` in this process and judges it.
    #[must_use]
    pub fn check(&self, code: PaneCode, build: &str) -> PaneCheck {
        let code = PrebuiltPane::new(code, build);
        run(
            &self.schema,
            &code,
            self.start.clone(),
            &self.inputs,
            &self.effects,
        )
        .1
    }

    /// The chain as a trace file under `code`, its head the state and chain this process's replay reaches.
    /// # Errors
    /// [`PaneTraceFault::Full`] past [`PANE_MAX_EVENTS`], or [`PaneTraceFault::Checkpoint`] or [`PaneTraceFault::Log`] for a state or input past the frame cap.
    pub fn trace(&self, code: PaneCode, build: &str) -> Result<PaneTrace, PaneTraceFault> {
        let prebuilt = PrebuiltPane::new(code, build);
        let identity = pane_identity(&prebuilt);
        let checkpoint = Checkpoint::capture(
            &self.schema,
            &self.start,
            Provenance {
                identity: identity.clone(),
                log_epoch: 0,
                input_cursor: 0,
                effect_cursor: 0,
                branch: BranchId(0),
                revision: Revision(0),
                parent: None,
            },
        );
        let mut log = EventLog::<PaneSchema>::new(
            LogHeader {
                identity: identity.clone(),
                log_epoch: 0,
                first_cursor: 0,
                ancestor: checkpoint.digest(),
            },
            PANE_MAX_EVENTS,
        );
        for i in &self.inputs {
            log.push(i.clone()).map_err(PaneTraceFault::Full)?;
        }
        let (_, check) = run(
            &self.schema,
            &prebuilt,
            self.start.clone(),
            &self.inputs,
            &self.effects,
        );
        Ok(PaneTrace {
            identity,
            checkpoint: checkpoint.encode().map_err(PaneTraceFault::Checkpoint)?,
            log: log.encode(&self.schema).map_err(PaneTraceFault::Log)?,
            effects: self.effects.clone(),
            head_state: check.state_digest,
            head_chain: check.observation_chain,
        })
    }
}

/// An exported pane chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneTrace {
    /// The identity it ran under.
    pub identity: TaskIdentity,
    /// The start state's checkpoint.
    pub checkpoint: Vec<u8>,
    /// The input log from it.
    pub log: Vec<u8>,
    /// The live engine's replies, one a slice.
    pub effects: Vec<EffectRecord>,
    /// The state the exporter's replay reached.
    pub head_state: Digest,
    /// The observation chain it observed.
    pub head_chain: Digest,
}

impl PaneTrace {
    /// The trace as one sealed file.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(
            self.checkpoint
                .len()
                .saturating_add(self.log.len())
                .saturating_add(self.effects.len().saturating_mul(64))
                .saturating_add(1024),
        );
        e.raw(&PANE_TRACE_MAGIC);
        e.identity(&self.identity);
        e.digest(&self.head_state);
        e.digest(&self.head_chain);
        e.bytes(&self.checkpoint);
        e.bytes(&self.log);
        e.u64(len_u64(self.effects.len()));
        for r in &self.effects {
            e.u64(r.seq);
            e.u32(r.target);
            e.u16(r.op);
            e.digest(&r.payload);
            e.bytes(&r.result);
        }
        let seal = Digest::of(PANE_TRACE_DOMAIN, &[e.as_bytes()]);
        e.digest(&seal);
        e.finish()
    }

    /// Whether `bytes` open as a pane trace file of any format.
    #[must_use]
    pub fn is_pane_trace(bytes: &[u8]) -> bool {
        bytes.get(..7) == Some(&PANE_TRACE_MAGIC[..7])
    }

    /// The trace a file holds.
    /// # Errors
    /// [`PaneTraceFault::Magic`], [`PaneTraceFault::Format`], [`PaneTraceFault::Seal`] or [`PaneTraceFault::Wire`].
    pub fn decode(bytes: &[u8]) -> Result<PaneTrace, PaneTraceFault> {
        if !Self::is_pane_trace(bytes) {
            return Err(PaneTraceFault::Magic);
        }
        let format = bytes.get(7).copied().ok_or(PaneTraceFault::Magic)?;
        if format != PANE_TRACE_MAGIC[7] {
            return Err(PaneTraceFault::Format { found: format });
        }
        let body_len = bytes
            .len()
            .checked_sub(SEAL_LEN)
            .filter(|&n| n >= PANE_TRACE_MAGIC.len())
            .ok_or(PaneTraceFault::Wire(WireFault::Truncated {
                field: "trace.seal",
                need: SEAL_LEN,
                have: bytes.len().saturating_sub(PANE_TRACE_MAGIC.len()),
            }))?;
        let (body, closing) = bytes.split_at(body_len);
        let mut sealed = [0u8; SEAL_LEN];
        sealed.copy_from_slice(closing);
        let (sealed, found) = (Digest(sealed), Digest::of(PANE_TRACE_DOMAIN, &[body]));
        if sealed != found {
            return Err(PaneTraceFault::Seal { sealed, found });
        }
        let mut d = Dec::new(&body[PANE_TRACE_MAGIC.len()..]);
        let read = (|| {
            let identity = d.identity()?;
            let head_state = d.digest("trace.head_state")?;
            let head_chain = d.digest("trace.head_chain")?;
            let checkpoint = d.bytes("trace.checkpoint")?.to_vec();
            let log = d.bytes("trace.log")?.to_vec();
            let n = d.count("trace.effects", len_u64(PANE_MAX_EVENTS))?;
            let mut effects = Vec::with_capacity(n.min(body.len() / 46));
            for _ in 0..n {
                effects.push(EffectRecord {
                    seq: d.u64("trace.effect.seq")?,
                    target: d.u32("trace.effect.target")?,
                    op: d.u16("trace.effect.op")?,
                    payload: d.digest("trace.effect.payload")?,
                    result: d.bytes("trace.effect.result")?.to_vec(),
                });
            }
            Ok(PaneTrace {
                identity,
                checkpoint,
                log,
                effects,
                head_state,
                head_chain,
            })
        })();
        let trace = read.map_err(PaneTraceFault::Wire)?;
        d.finish().map_err(PaneTraceFault::Wire)?;
        Ok(trace)
    }

    /// Refuses a trace whose identity no code of `build` runs, then replays it from its checkpoint on `origin` under that code, its replies shadowed by the recorded ones, judges it by `pane.faithful-replay`, and checks the replay reached the exporter's head.
    /// # Errors
    /// [`PaneTraceFault::Identity`], [`PaneTraceFault::Checkpoint`], [`PaneTraceFault::Restore`], [`PaneTraceFault::Log`], [`PaneTraceFault::Tail`], [`PaneTraceFault::State`] or [`PaneTraceFault::Chain`], in that order.
    pub fn check(&self, origin: ClockReading, build: &str) -> Result<PaneCheck, PaneTraceFault> {
        let ours = |c: PaneCode| pane_identity(&PrebuiltPane::new(c, build));
        let code = PaneCode::ALL
            .iter()
            .copied()
            .find(|&c| ours(c) == self.identity)
            .ok_or_else(|| {
                PaneTraceFault::Identity(
                    IdentityMismatch::first(&self.identity, &ours(PaneCode::ThisBuild))
                        .unwrap_or(IdentityMismatch::Code),
                )
            })?;
        let schema = PaneSchema::new(origin);
        let checkpoint =
            Checkpoint::decode(&self.checkpoint).map_err(PaneTraceFault::Checkpoint)?;
        let start = checkpoint
            .restore(&schema, &self.identity)
            .map_err(PaneTraceFault::Restore)?;
        let (log, tail) = EventLog::decode(
            &schema,
            &self.log,
            &self.identity,
            &checkpoint.digest(),
            PANE_MAX_EVENTS,
        )
        .map_err(PaneTraceFault::Log)?;
        if tail != Tail::Clean {
            return Err(PaneTraceFault::Tail(tail));
        }
        let (_, check) = run(
            &schema,
            &PrebuiltPane::new(code, build),
            start,
            log.inputs(),
            &self.effects,
        );
        if check.state_digest != self.head_state {
            return Err(PaneTraceFault::State {
                recorded: self.head_state,
                replayed: check.state_digest,
            });
        }
        if check.observation_chain != self.head_chain {
            return Err(PaneTraceFault::Chain {
                recorded: self.head_chain,
                replayed: check.observation_chain,
            });
        }
        Ok(check)
    }
}
