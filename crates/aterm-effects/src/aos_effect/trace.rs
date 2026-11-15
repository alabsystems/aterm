// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An exported effect trace as one file, and its replay in another process.

use core::fmt;
use core::marker::PhantomData;

use aterm_aos::checkpoint::{Checkpoint, CheckpointFault, RestoreRefusal};
use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::NoEffects;
use aterm_aos::log::{EventLog, IdentityMismatch, LogFault, Tail};
use aterm_aos::replay::{FoldedRun, replay_fold};
use aterm_aos::task::{Code, StepRefusal};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{Digest, TaskIdentity};
use aterm_time::Instant;

use super::{EffectCode, EffectSchema, HostedEngine};

const SEAL_LEN: usize = 32;

/// The five artifacts a hosted engine's task runs under, with `code` running it.
#[inline]
#[must_use]
pub fn identity_of<E: HostedEngine>(code: &EffectCode<E>) -> TaskIdentity {
    TaskIdentity {
        task: E::task_artifact(),
        code: code.id().clone(),
        contract: E::contract().id().clone(),
        schema: E::schema_artifact(),
        profile: E::profile_artifact(),
    }
}

/// An exported trace: the identity it was recorded under, the ancestor checkpoint, the log since
/// it and the live head when it was exported.
pub struct EffectTrace<E: HostedEngine> {
    pub identity: TaskIdentity,
    pub checkpoint: Vec<u8>,
    pub log: Vec<u8>,
    /// The live state's digest when the trace was exported.
    pub head_state: Digest,
    /// The live observation chain since the checkpoint when the trace was exported.
    pub head_chain: Digest,
    pub(crate) _e: PhantomData<fn() -> E>,
}

impl<E: HostedEngine> Clone for EffectTrace<E> {
    #[inline]
    fn clone(&self) -> Self {
        EffectTrace {
            identity: self.identity.clone(),
            checkpoint: self.checkpoint.clone(),
            log: self.log.clone(),
            head_state: self.head_state,
            head_chain: self.head_chain,
            _e: PhantomData,
        }
    }
}

impl<E: HostedEngine> PartialEq for EffectTrace<E> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.checkpoint == other.checkpoint
            && self.log == other.log
            && self.head_state == other.head_state
            && self.head_chain == other.head_chain
    }
}

impl<E: HostedEngine> Eq for EffectTrace<E> {}

impl<E: HostedEngine> fmt::Debug for EffectTrace<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectTrace")
            .field("task", &E::SHORT_NAME)
            .field("identity", &self.identity)
            .field("checkpoint", &self.checkpoint.len())
            .field("log", &self.log.len())
            .field("head_state", &self.head_state)
            .field("head_chain", &self.head_chain)
            .finish()
    }
}

/// A trace decoded on a reader's epoch ([`EffectTrace::read`]).
pub struct ReadTrace<E: HostedEngine> {
    pub schema: EffectSchema<E>,
    pub checkpoint: Checkpoint,
    pub log: EventLog<EffectSchema<E>>,
    pub counts: E::Counts,
}

/// What [`EffectTrace::replay`] reached.
pub struct TraceReplay<E: HostedEngine> {
    /// The code the identity names.
    pub code: E::Prebuilt,
    /// The ancestor checkpoint's digest.
    pub checkpoint: Digest,
    pub log_epoch: u64,
    pub first_cursor: u64,
    pub counts: E::Counts,
    /// The replayed head state's count of keys logged unseen ([`HostedEngine::unseen`]): the
    /// checkpoint's own count included, so a trace cut after a re-anchor or a repair still says
    /// it holds such keys.
    pub unseen: u32,
    pub state_digest: Digest,
    pub observation_chain: Digest,
}

impl<E: HostedEngine> Clone for TraceReplay<E> {
    #[inline]
    fn clone(&self) -> Self {
        TraceReplay {
            code: self.code,
            checkpoint: self.checkpoint,
            log_epoch: self.log_epoch,
            first_cursor: self.first_cursor,
            counts: self.counts,
            unseen: self.unseen,
            state_digest: self.state_digest,
            observation_chain: self.observation_chain,
        }
    }
}

impl<E: HostedEngine> PartialEq for TraceReplay<E> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.checkpoint == other.checkpoint
            && self.log_epoch == other.log_epoch
            && self.first_cursor == other.first_cursor
            && self.counts == other.counts
            && self.unseen == other.unseen
            && self.state_digest == other.state_digest
            && self.observation_chain == other.observation_chain
    }
}

impl<E: HostedEngine> Eq for TraceReplay<E> {}

impl<E: HostedEngine> fmt::Debug for TraceReplay<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceReplay")
            .field("code", &self.code)
            .field("checkpoint", &self.checkpoint)
            .field("log_epoch", &self.log_epoch)
            .field("first_cursor", &self.first_cursor)
            .field("counts", &self.counts)
            .field("unseen", &self.unseen)
            .field("state_digest", &self.state_digest)
            .field("observation_chain", &self.observation_chain)
            .finish()
    }
}

/// What [`EffectTrace::check`] found: the replay and the contract's verdict on it.
pub struct TraceCheck<E: HostedEngine> {
    pub replay: TraceReplay<E>,
    pub verdict: Verdict,
}

impl<E: HostedEngine> Clone for TraceCheck<E> {
    #[inline]
    fn clone(&self) -> Self {
        TraceCheck {
            replay: self.replay.clone(),
            verdict: self.verdict.clone(),
        }
    }
}

impl<E: HostedEngine> PartialEq for TraceCheck<E> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.replay == other.replay && self.verdict == other.verdict
    }
}

impl<E: HostedEngine> Eq for TraceCheck<E> {}

impl<E: HostedEngine> fmt::Debug for TraceCheck<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceCheck")
            .field("replay", &self.replay)
            .field("verdict", &self.verdict)
            .finish()
    }
}

/// Why a trace file was not read or did not replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceFault {
    /// The trace's identity is none this build runs the task under: the first artifact that differs.
    Identity(IdentityMismatch),
    /// The bytes do not open with the task's magic.
    Magic,
    /// They open with another format byte than the task's magic's.
    Format { found: u8 },
    /// The closing digest does not name the bytes before it.
    Seal { sealed: Digest, found: Digest },
    /// A field of the file does not decode, or bytes follow its end.
    Wire(WireFault),
    /// The ancestor checkpoint does not decode.
    Checkpoint(CheckpointFault),
    /// The log does not decode beside the identity and the checkpoint.
    Log(LogFault),
    /// The log decoded only in part.
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

impl TraceFault {
    /// The refusal as one word (`identity`, `magic`, `state`, …).
    #[inline]
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            TraceFault::Identity(_) => "identity",
            TraceFault::Magic => "magic",
            TraceFault::Format { .. } => "format",
            TraceFault::Seal { .. } => "seal",
            TraceFault::Wire(_) => "wire",
            TraceFault::Checkpoint(_) => "checkpoint",
            TraceFault::Log(_) => "log",
            TraceFault::Tail(_) => "tail",
            TraceFault::Broken { .. } => "broken",
            TraceFault::UnknownCode { .. } => "unknown-code",
            TraceFault::Restore(_) => "restore",
            TraceFault::Refused { .. } => "refused",
            TraceFault::State { .. } => "state",
            TraceFault::Chain { .. } => "chain",
        }
    }
}

impl fmt::Display for TraceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceFault::Identity(m) => write!(
                f,
                "the trace was recorded under an identity this build does not run: {m}"
            ),
            TraceFault::Magic => f.write_str("the bytes are not a momentum trace file"),
            TraceFault::Format { found } => write!(
                f,
                "the trace file is format {found}; this build reads format 1"
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

/// Whether `bytes` open with `magic`'s seven bytes (any format byte).
#[inline]
#[must_use]
pub(crate) fn opens_with(bytes: &[u8], magic: &[u8; 8]) -> bool {
    bytes.get(..7) == Some(&magic[..7])
}

impl<E: HostedEngine> EffectTrace<E> {
    /// A trace of `identity` over `checkpoint` and `log`, exported at the head (`head_state`, `head_chain`).
    #[inline]
    #[must_use]
    pub const fn new(
        identity: TaskIdentity,
        checkpoint: Vec<u8>,
        log: Vec<u8>,
        head_state: Digest,
        head_chain: Digest,
    ) -> Self {
        EffectTrace {
            identity,
            checkpoint,
            log,
            head_state,
            head_chain,
            _e: PhantomData,
        }
    }

    /// Whether `bytes` open as this task's trace file (any format byte).
    #[inline]
    #[must_use]
    pub fn is_trace_file(bytes: &[u8]) -> bool {
        opens_with(bytes, &E::TRACE_MAGIC)
    }

    /// The trace as one file.
    #[inline]
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::with_capacity(
            self.checkpoint
                .len()
                .saturating_add(self.log.len())
                .saturating_add(1024),
        );
        e.raw(&E::TRACE_MAGIC);
        e.identity(&self.identity);
        e.digest(&self.head_state);
        e.digest(&self.head_chain);
        e.bytes(&self.checkpoint);
        e.bytes(&self.log);
        let seal = Digest::of(E::TRACE_DOMAIN, &[e.as_bytes()]);
        e.digest(&seal);
        e.finish()
    }

    /// The trace a file holds.
    /// # Errors
    /// [`TraceFault::Magic`], [`TraceFault::Format`], [`TraceFault::Seal`] or [`TraceFault::Wire`].
    #[inline]
    pub fn decode(bytes: &[u8]) -> Result<Self, TraceFault> {
        let magic_len = E::TRACE_MAGIC.len();
        let magic = bytes.get(..magic_len).ok_or(TraceFault::Magic)?;
        if magic[..7] != E::TRACE_MAGIC[..7] {
            return Err(TraceFault::Magic);
        }
        if magic[7] != E::TRACE_MAGIC[7] {
            return Err(TraceFault::Format { found: magic[7] });
        }
        if bytes.len() < magic_len + SEAL_LEN {
            return Err(TraceFault::Wire(WireFault::Truncated {
                field: "trace.seal",
                need: SEAL_LEN,
                have: bytes.len() - magic_len,
            }));
        }
        let body_len = bytes.len() - SEAL_LEN;
        let (body, closing) = bytes.split_at(body_len);
        let mut sealed = [0u8; SEAL_LEN];
        sealed.copy_from_slice(closing);
        let (sealed, found) = (Digest(sealed), Digest::of(E::TRACE_DOMAIN, &[body]));
        if sealed != found {
            return Err(TraceFault::Seal { sealed, found });
        }
        let mut d = Dec::new(&body[magic_len..]);
        let identity = d.identity().map_err(TraceFault::Wire)?;
        let head_state = d.digest("trace.head_state").map_err(TraceFault::Wire)?;
        let head_chain = d.digest("trace.head_chain").map_err(TraceFault::Wire)?;
        let checkpoint = d
            .bytes("trace.checkpoint")
            .map_err(TraceFault::Wire)?
            .to_vec();
        let log = d.bytes("trace.log").map_err(TraceFault::Wire)?.to_vec();
        d.finish().map_err(TraceFault::Wire)?;
        Ok(Self::new(identity, checkpoint, log, head_state, head_chain))
    }

    /// The checkpoint and the log decoded on the reader's recording `epoch`.
    /// # Errors
    /// [`TraceFault::Checkpoint`], [`TraceFault::Log`], or [`TraceFault::Tail`] for a torn or
    /// unsealed log.
    #[inline]
    pub fn read(&self, epoch: Instant) -> Result<ReadTrace<E>, TraceFault> {
        self.read_at_most(epoch, E::MAX_EVENTS)
    }

    /// How many keys the trace holds that were logged unseen: its checkpoint state's count
    /// ([`HostedEngine::unseen`]) and its log's ([`HostedEngine::unseen_logged`]), so a trace cut
    /// after a re-anchor or a repair still says it holds them.
    /// # Errors
    /// Any of [`Self::read`]'s, or [`TraceFault::Restore`] for a checkpoint of another identity.
    #[inline]
    pub fn unseen_keys(&self, epoch: Instant) -> Result<u32, TraceFault> {
        let read = self.read(epoch)?;
        let start: E::State = read
            .checkpoint
            .restore(&read.schema, &self.identity)
            .map_err(TraceFault::Restore)?;
        let logged = u32::try_from(E::unseen_logged(&read.counts)).unwrap_or(u32::MAX);
        Ok(E::unseen(&start).saturating_add(logged))
    }

    /// [`Self::read`] refusing, before it decodes an event, a log of more than `max_events` inputs.
    #[inline]
    fn read_at_most(&self, epoch: Instant, max_events: usize) -> Result<ReadTrace<E>, TraceFault> {
        let schema = EffectSchema::<E>::new(epoch);
        let checkpoint = Checkpoint::decode(&self.checkpoint).map_err(TraceFault::Checkpoint)?;
        let (log, tail) = EventLog::decode(
            &schema,
            &self.log,
            &self.identity,
            &checkpoint.digest(),
            max_events,
        )
        .map_err(TraceFault::Log)?;
        if tail != Tail::Clean {
            return Err(TraceFault::Tail(tail));
        }
        let counts = E::counts(log.inputs());
        Ok(ReadTrace {
            schema,
            checkpoint,
            log,
            counts,
        })
    }

    /// The first artifact where the trace's identity differs from the shipped code's identity in `build`, or `None` when it is the identity of a code this build carries.
    #[inline]
    #[must_use]
    pub fn identity_mismatch(&self, build: &str) -> Option<IdentityMismatch> {
        let Some(first) = E::ALL.first() else {
            return Some(IdentityMismatch::Code);
        };
        let shipped = identity_of(&EffectCode::<E>::prebuilt(*first, build));
        if shipped == self.identity {
            return None;
        }
        // Every code shares the shipped one's task, contract, schema and profile: only the code
        // artifact is compared for the rest.
        if shipped.task == self.identity.task
            && shipped.contract == self.identity.contract
            && shipped.schema == self.identity.schema
            && shipped.profile == self.identity.profile
        {
            let mut i = 1;
            while i < E::ALL.len() {
                if E::code_artifact(E::code_of(E::ALL[i]), build) == self.identity.code {
                    return None;
                }
                i += 1;
            }
        }
        Some(IdentityMismatch::first(&self.identity, &shipped).unwrap_or(IdentityMismatch::Code))
    }

    /// Refuses a trace whose identity this build does not run ([`TraceFault::Identity`]) or whose log holds more inputs than one host log epoch ([`HostedEngine::HOST_MAX_EVENTS`]), then replays it as [`Self::replay`] does and judges the run by the identity's contract.
    /// # Errors
    /// [`TraceFault::Identity`], [`TraceFault::Log`] for a log past one host epoch, then any of [`Self::replay`]'s.
    #[inline]
    pub fn check(&self, epoch: Instant, build: &str) -> Result<TraceCheck<E>, TraceFault> {
        if let Some(m) = self.identity_mismatch(build) {
            return Err(TraceFault::Identity(m));
        }
        let (replay, verdict) = self.replayed_with(
            epoch,
            build,
            E::HOST_MAX_EVENTS,
            |read, code, start, first| {
                E::contract().replay_judged(
                    &read.schema,
                    code,
                    start,
                    read.log.inputs(),
                    first,
                    &mut NoEffects,
                )
            },
        )?;
        Ok(TraceCheck { replay, verdict })
    }

    /// Reads the trace on `epoch`, replays its log from its checkpoint, and checks the replay
    /// reached the recorded head.
    /// # Errors
    /// Any of [`Self::read`]'s, then [`TraceFault::Broken`], [`TraceFault::UnknownCode`],
    /// [`TraceFault::Restore`], [`TraceFault::Refused`], [`TraceFault::State`] or
    /// [`TraceFault::Chain`], in that order.
    #[inline]
    pub fn replay(&self, epoch: Instant, build: &str) -> Result<TraceReplay<E>, TraceFault> {
        let (replay, ()) =
            self.replayed_with(epoch, build, E::MAX_EVENTS, |read, code, start, first| {
                (
                    replay_fold(
                        &read.schema,
                        code,
                        start,
                        read.log.inputs(),
                        first,
                        &mut NoEffects,
                        |_, _, _, _| {},
                    ),
                    (),
                )
            })?;
        Ok(replay)
    }

    /// [`Self::replay`] of a log of at most `max_events` inputs, folding observations through `run_fn`.
    #[inline]
    fn replayed_with<T>(
        &self,
        epoch: Instant,
        build: &str,
        max_events: usize,
        run_fn: impl FnOnce(
            &ReadTrace<E>,
            &EffectCode<E>,
            E::State,
            u64,
        ) -> (FoldedRun<EffectSchema<E>>, T),
    ) -> Result<(TraceReplay<E>, T), TraceFault> {
        let read = self.read_at_most(epoch, max_events)?;
        if let Some(why) = read.log.broken() {
            return Err(TraceFault::Broken { why });
        }
        let mut found = None;
        let mut ci = 0;
        while ci < E::ALL.len() {
            let c = E::ALL[ci];
            let p = EffectCode::<E>::prebuilt(c, build);
            if p.id == self.identity.code {
                found = Some((c, p));
                break;
            }
            ci += 1;
        }
        let Some((named, code)) = found else {
            return Err(TraceFault::UnknownCode {
                name: self.identity.code.name.clone(),
                build: build.to_owned(),
            });
        };
        let start = read
            .checkpoint
            .restore(&read.schema, &self.identity)
            .map_err(TraceFault::Restore)?;
        let first_cursor = read.log.header().first_cursor;
        let (run, extra) = run_fn(&read, &code, start, first_cursor);
        if let Some((cursor, refusal)) = &run.refused {
            return Err(TraceFault::Refused {
                cursor: *cursor,
                refusal: refusal.clone(),
            });
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
        let replay = TraceReplay {
            code: named,
            checkpoint: read.checkpoint.digest(),
            log_epoch: read.log.header().log_epoch,
            first_cursor,
            counts: read.counts,
            unseen: E::unseen(&run.state),
            state_digest: run.state_digest,
            observation_chain: run.observation_chain,
        };
        Ok((replay, extra))
    }
}
