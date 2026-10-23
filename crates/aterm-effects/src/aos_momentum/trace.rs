// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An exported momentum trace as one file, and its replay in another process.

use core::fmt;

use aterm_aos::checkpoint::{Checkpoint, CheckpointFault, RestoreRefusal};
use aterm_aos::effects::NoEffects;
use aterm_aos::log::{EventLog, LogFault, Tail};
use aterm_aos::replay::replay;
use aterm_aos::task::StepRefusal;
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{Digest, TaskIdentity};
use aterm_time::Instant;

use super::{MOMENTUM_MAX_EVENTS, MomentumCode, MomentumEvent, MomentumSchema, PrebuiltMomentum};

/// The bytes that open a momentum trace file: `AOSMTR\0` then the format byte, 1.
pub const TRACE_FILE_MAGIC: [u8; 8] = *b"AOSMTR\0\x01";

/// The domain of the digest that closes a trace file.
pub const TRACE_FILE_DOMAIN: &[u8] = b"aterm-aos-momentum-trace-file-v1\0";

/// The closing digest's length.
const SEAL_LEN: usize = 32;

/// An exported trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MomentumTrace {
    pub identity: TaskIdentity,
    pub checkpoint: Vec<u8>,
    pub log: Vec<u8>,
    /// The live state's digest when the trace was exported.
    pub head_state: Digest,
    /// The live observation chain since the checkpoint when the trace was exported.
    pub head_chain: Digest,
}

/// How many inputs of each kind a log holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventCounts {
    pub keys: usize,
    pub resets: usize,
    pub ticks: usize,
    pub queries: usize,
}

impl EventCounts {
    /// The counts over `inputs`.
    #[inline]
    #[must_use]
    pub fn of(inputs: &[MomentumEvent]) -> Self {
        let mut c = EventCounts::default();
        for input in inputs {
            let slot = match input {
                MomentumEvent::Key { .. } => &mut c.keys,
                MomentumEvent::Reset => &mut c.resets,
                MomentumEvent::Tick { .. } => &mut c.ticks,
                MomentumEvent::Query { .. } => &mut c.queries,
            };
            *slot = slot.saturating_add(1);
        }
        c
    }

    /// Every input counted.
    #[inline]
    #[must_use]
    pub const fn total(&self) -> usize {
        self.keys
            .saturating_add(self.resets)
            .saturating_add(self.ticks)
            .saturating_add(self.queries)
    }
}

/// A trace decoded on a reader's epoch ([`MomentumTrace::read`]).
pub struct ReadTrace {
    pub schema: MomentumSchema,
    pub checkpoint: Checkpoint,
    pub log: EventLog<MomentumSchema>,
    pub counts: EventCounts,
}

/// What [`MomentumTrace::replay`] reached: the recorded head, reproduced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceReplay {
    /// The code the identity names.
    pub code: MomentumCode,
    /// The ancestor checkpoint's digest.
    pub checkpoint: Digest,
    pub log_epoch: u64,
    pub first_cursor: u64,
    pub counts: EventCounts,
    pub state_digest: Digest,
    pub observation_chain: Digest,
}

/// Why a trace file was not read or did not replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceFault {
    /// The bytes do not open with `AOSMTR\0`.
    Magic,
    /// They open with another format byte than [`TRACE_FILE_MAGIC`]'s.
    Format { found: u8 },
    /// The closing digest does not name the bytes before it: the file was cut or altered.
    Seal { sealed: Digest, found: Digest },
    /// A field of the file does not decode, or bytes follow its end.
    Wire(WireFault),
    /// The ancestor checkpoint does not decode.
    Checkpoint(CheckpointFault),
    /// The log does not decode beside the identity and the checkpoint.
    Log(LogFault),
    /// The log decoded only in part: torn, or not ended by its seal.
    Tail(Tail),
    /// The log says a delivered input is missing from it, so no replay of it is the live run.
    Broken { why: &'static str },
    /// No code compiled into this build under `build` is the code the identity names.
    UnknownCode { name: String, build: String },
    /// The checkpoint is not handed to the identity's task.
    Restore(RestoreRefusal),
    /// The replay stopped at an input the code refused.
    Refused { cursor: u64, refusal: StepRefusal },
    /// The replay reached another state than the recorded head's.
    State { recorded: Digest, replayed: Digest },
    /// The replay observed another chain than the recorded head's.
    Chain { recorded: Digest, replayed: Digest },
}

impl fmt::Display for TraceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceFault::Magic => f.write_str("the bytes are not a momentum trace file"),
            TraceFault::Format { found } => write!(
                f,
                "the trace file is format {found}; this build reads format {}",
                TRACE_FILE_MAGIC[7]
            ),
            TraceFault::Seal { sealed, found } => write!(
                f,
                "the trace file closes with the digest {}, but its bytes digest to {}: it was cut or altered",
                sealed.short(),
                found.short()
            ),
            TraceFault::Wire(w) => write!(f, "the trace file does not decode: {w}"),
            TraceFault::Checkpoint(c) => write!(f, "the ancestor checkpoint does not decode: {c}"),
            TraceFault::Log(l) => write!(f, "the log does not decode: {l}"),
            TraceFault::Tail(t) => write!(f, "the log is not whole: {t:?}"),
            TraceFault::Broken { why } => {
                write!(f, "the log is broken ({why}): a delivered input is missing")
            }
            TraceFault::UnknownCode { name, build } => write!(
                f,
                "no code of the build `{build}` is the trace's code `{name}`"
            ),
            TraceFault::Restore(r) => write!(f, "the checkpoint does not restore: {r}"),
            TraceFault::Refused { cursor, refusal } => {
                write!(
                    f,
                    "the replay refused the input at cursor {cursor}: {refusal}"
                )
            }
            TraceFault::State { recorded, replayed } => write!(
                f,
                "the replay reached the state {}, not the recorded head's {}",
                replayed.short(),
                recorded.short()
            ),
            TraceFault::Chain { recorded, replayed } => write!(
                f,
                "the replay observed the chain {}, not the recorded head's {}",
                replayed.short(),
                recorded.short()
            ),
        }
    }
}

impl std::error::Error for TraceFault {}

impl MomentumTrace {
    /// The trace as one file.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(
            self.checkpoint
                .len()
                .saturating_add(self.log.len())
                .saturating_add(1024),
        );
        e.raw(&TRACE_FILE_MAGIC);
        e.identity(&self.identity);
        e.digest(&self.head_state);
        e.digest(&self.head_chain);
        e.bytes(&self.checkpoint);
        e.bytes(&self.log);
        let seal = Digest::of(TRACE_FILE_DOMAIN, &[e.as_bytes()]);
        e.digest(&seal);
        e.finish()
    }

    /// The trace a file holds.
    /// # Errors
    /// [`TraceFault::Magic`], [`TraceFault::Format`], [`TraceFault::Seal`] or [`TraceFault::Wire`].
    pub fn decode(bytes: &[u8]) -> Result<MomentumTrace, TraceFault> {
        let magic = bytes
            .get(..TRACE_FILE_MAGIC.len())
            .ok_or(TraceFault::Magic)?;
        if magic[..7] != TRACE_FILE_MAGIC[..7] {
            return Err(TraceFault::Magic);
        }
        if magic[7] != TRACE_FILE_MAGIC[7] {
            return Err(TraceFault::Format { found: magic[7] });
        }
        let body_len = bytes
            .len()
            .checked_sub(SEAL_LEN)
            .filter(|&n| n >= TRACE_FILE_MAGIC.len())
            .ok_or(TraceFault::Wire(WireFault::Truncated {
                field: "trace.seal",
                need: SEAL_LEN,
                have: bytes.len() - TRACE_FILE_MAGIC.len(),
            }))?;
        let (body, closing) = bytes.split_at(body_len);
        let mut sealed = [0u8; SEAL_LEN];
        sealed.copy_from_slice(closing);
        let (sealed, found) = (Digest(sealed), Digest::of(TRACE_FILE_DOMAIN, &[body]));
        if sealed != found {
            return Err(TraceFault::Seal { sealed, found });
        }
        let mut d = Dec::new(&body[TRACE_FILE_MAGIC.len()..]);
        let read = (|| {
            let identity = d.identity()?;
            let head_state = d.digest("trace.head_state")?;
            let head_chain = d.digest("trace.head_chain")?;
            let checkpoint = d.bytes("trace.checkpoint")?.to_vec();
            let log = d.bytes("trace.log")?.to_vec();
            Ok(MomentumTrace {
                identity,
                checkpoint,
                log,
                head_state,
                head_chain,
            })
        })();
        let trace = read.map_err(TraceFault::Wire)?;
        d.finish().map_err(TraceFault::Wire)?;
        Ok(trace)
    }

    /// The checkpoint and the log decoded on the reader's recording `epoch`.
    /// # Errors
    /// [`TraceFault::Checkpoint`], [`TraceFault::Log`], or [`TraceFault::Tail`] for a torn or
    /// unsealed log.
    pub fn read(&self, epoch: Instant) -> Result<ReadTrace, TraceFault> {
        let schema = MomentumSchema::new(epoch);
        let checkpoint = Checkpoint::decode(&self.checkpoint).map_err(TraceFault::Checkpoint)?;
        let (log, tail) = EventLog::decode(
            &schema,
            &self.log,
            &self.identity,
            &checkpoint.digest(),
            MOMENTUM_MAX_EVENTS,
        )
        .map_err(TraceFault::Log)?;
        if tail != Tail::Clean {
            return Err(TraceFault::Tail(tail));
        }
        let counts = EventCounts::of(log.inputs());
        Ok(ReadTrace {
            schema,
            checkpoint,
            log,
            counts,
        })
    }

    /// Reads the trace on `epoch`, replays its log from its checkpoint through the code of `build`
    /// the identity names, and checks the replay reached the recorded head.
    /// # Errors
    /// Any of [`Self::read`]'s, then [`TraceFault::Broken`], [`TraceFault::UnknownCode`],
    /// [`TraceFault::Restore`], [`TraceFault::Refused`], [`TraceFault::State`] or
    /// [`TraceFault::Chain`], in that order.
    pub fn replay(&self, epoch: Instant, build: &str) -> Result<TraceReplay, TraceFault> {
        let read = self.read(epoch)?;
        if let Some(why) = read.log.broken() {
            return Err(TraceFault::Broken { why });
        }
        let code = MomentumCode::ALL
            .iter()
            .map(|&c| PrebuiltMomentum::new(c, build))
            .find(|p| p.id == self.identity.code)
            .ok_or_else(|| TraceFault::UnknownCode {
                name: self.identity.code.name.clone(),
                build: build.to_owned(),
            })?;
        let start = read
            .checkpoint
            .restore(&read.schema, &self.identity)
            .map_err(TraceFault::Restore)?;
        let first_cursor = read.log.header().first_cursor;
        let run = replay(
            &read.schema,
            &code,
            start,
            read.log.inputs(),
            first_cursor,
            &mut NoEffects,
        );
        if let Some((cursor, refusal)) = run.refused {
            return Err(TraceFault::Refused { cursor, refusal });
        }
        if run.state_digest != self.head_state {
            return Err(TraceFault::State {
                recorded: self.head_state,
                replayed: run.state_digest,
            });
        }
        if run.observation_chain != self.head_chain {
            return Err(TraceFault::Chain {
                recorded: self.head_chain,
                replayed: run.observation_chain,
            });
        }
        Ok(TraceReplay {
            code: code.code,
            checkpoint: read.checkpoint.digest(),
            log_epoch: read.log.header().log_epoch,
            first_cursor,
            counts: read.counts,
            state_digest: run.state_digest,
            observation_chain: run.observation_chain,
        })
    }
}
