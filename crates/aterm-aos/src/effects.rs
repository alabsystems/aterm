// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effect frontier (plan §3): how a task's external effects are recorded live and
//! compared in shadow replay.
//!
//! # The law
//!
//! Every external effect a step proposes is an [`EffectRequest`]: a target capability, an
//! operation and an immutable payload. The record pass ([`RecordingPort`]) hands each request
//! to the host's live sink and keeps an [`EffectRecord`] of it — its sequence number, target,
//! op, the digest of its payload, and the result the task observed.
//!
//! Shadow replay ([`ShadowPort`]) never submits anything: it holds no live sink, so it cannot.
//! Each request is compared with the next recorded one — target, op and payload digest —
//! **before** the recorded result is handed back. The first mismatch is the *frontier*: the
//! point where continuing would need a new external action. The frontier is sticky: every
//! later request is answered with the same divergence, even one that would match a later
//! record, and nothing past it is ever answered from the recording. A run that ends with
//! records it never asked for has *deleted* an effect, and [`ShadowPort::finish`] refuses it
//! ([`EffectDivergence::Unreached`]) — omitting an earlier effect is a divergence even when
//! nothing new was submitted.
//!
//! A task that declares no external effects runs with [`NoEffects`]: any submission is
//! [`EffectDivergence::Undeclared`].
//!
//! Positions are 0-based indices into the shadowed sequence: the `n`-th request of the run.

use core::fmt;

use crate::ident::Digest;

/// The domain under which an effect payload is digested.
pub const EFFECT_PAYLOAD_DOMAIN: &[u8] = b"aterm-aos-effect-payload-v1\0";

/// One external effect a step proposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectRequest {
    /// The target capability (which door the effect goes through).
    pub target: u32,
    /// The operation on that target.
    pub op: u16,
    /// The immutable payload.
    pub payload: Vec<u8>,
}

impl EffectRequest {
    /// The payload's digest, as an [`EffectRecord`] keeps it.
    #[inline]
    #[must_use]
    pub fn payload_digest(&self) -> Digest {
        Digest::of(EFFECT_PAYLOAD_DOMAIN, &[&self.payload])
    }
}

/// One recorded effect: what was asked and what the task observed in return.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectRecord {
    /// Its place in the recorded sequence.
    pub seq: u64,
    /// The target capability.
    pub target: u32,
    /// The operation.
    pub op: u16,
    /// The digest of the payload ([`EffectRequest::payload_digest`]).
    pub payload: Digest,
    /// The result the task observed.
    pub result: Vec<u8>,
}

/// Where a step submits its external effects.
pub trait EffectPort {
    /// Submits one request and returns the result the task observes, or the divergence that
    /// stopped it.
    fn submit(&mut self, req: EffectRequest) -> Result<Vec<u8>, EffectDivergence>;
}

/// Why a run's effects are not the recorded ones. Nothing was submitted live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectDivergence {
    /// The run asked for an effect past the end of the recording (an insertion).
    Inserted {
        /// The request's position.
        position: u64,
    },
    /// The request names another target than the record at its position.
    TargetDiverged {
        /// The position.
        position: u64,
        /// The recorded target.
        recorded: u32,
        /// The target asked for.
        asked: u32,
    },
    /// The request names another operation than the record at its position.
    OpDiverged {
        /// The position.
        position: u64,
        /// The recorded op.
        recorded: u16,
        /// The op asked for.
        asked: u16,
    },
    /// The request carries another payload than the record at its position.
    PayloadDiverged {
        /// The position.
        position: u64,
        /// The recorded payload digest.
        recorded: Digest,
        /// The asked payload's digest.
        asked: Digest,
    },
    /// The run ended with recorded effects it never asked for (a deletion), seen at
    /// [`ShadowPort::finish`].
    Unreached {
        /// The first position never asked for.
        position: u64,
        /// How many records were never asked for.
        remaining: u64,
    },
    /// A task that declares no external effects submitted one.
    Undeclared {
        /// The target it asked for.
        target: u32,
    },
}

impl fmt::Display for EffectDivergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EffectDivergence::Inserted { position } => write!(
                f,
                "effect {position} was asked for past the end of the recording"
            ),
            EffectDivergence::TargetDiverged {
                position,
                recorded,
                asked,
            } => write!(
                f,
                "effect {position} asked for target {asked} where target {recorded} was recorded"
            ),
            EffectDivergence::OpDiverged {
                position,
                recorded,
                asked,
            } => write!(
                f,
                "effect {position} asked for op {asked} where op {recorded} was recorded"
            ),
            EffectDivergence::PayloadDiverged {
                position,
                recorded,
                asked,
            } => write!(
                f,
                "effect {position} carries payload {} where payload {} was recorded",
                asked.short(),
                recorded.short()
            ),
            EffectDivergence::Unreached {
                position,
                remaining,
            } => write!(
                f,
                "the run ended with {remaining} recorded effects from position {position} never asked for"
            ),
            EffectDivergence::Undeclared { target } => write!(
                f,
                "a task that declares no external effects asked for target {target}"
            ),
        }
    }
}

impl std::error::Error for EffectDivergence {}

/// Shadow replay: compares each request with the next recorded one **before** handing back
/// its recorded result; holds no live sink, so it cannot submit anything.
#[derive(Clone, Debug)]
pub struct ShadowPort<'a> {
    recorded: &'a [EffectRecord],
    next: usize,
    frontier: Option<EffectDivergence>,
}

impl<'a> ShadowPort<'a> {
    /// A shadow over `recorded`, positioned at its first record.
    #[inline]
    #[must_use]
    pub const fn new(recorded: &'a [EffectRecord]) -> Self {
        ShadowPort {
            recorded,
            next: 0,
            frontier: None,
        }
    }

    /// The first divergence, sticky.
    #[inline]
    #[must_use]
    pub const fn frontier(&self) -> Option<&EffectDivergence> {
        self.frontier.as_ref()
    }

    /// How many requests matched their records.
    #[inline]
    #[must_use]
    pub fn matched(&self) -> u64 {
        crate::ident::len_u64(self.next)
    }

    /// Ends the shadow: the number of matched effects, or the frontier, or
    /// [`EffectDivergence::Unreached`] if records remain.
    pub fn finish(self) -> Result<u64, EffectDivergence> {
        if let Some(d) = self.frontier {
            return Err(d);
        }
        let remaining = self.recorded.len().saturating_sub(self.next);
        if remaining > 0 {
            return Err(EffectDivergence::Unreached {
                position: crate::ident::len_u64(self.next),
                remaining: crate::ident::len_u64(remaining),
            });
        }
        Ok(crate::ident::len_u64(self.next))
    }

    fn compare(&self, req: &EffectRequest) -> Result<&'a EffectRecord, EffectDivergence> {
        let position = crate::ident::len_u64(self.next);
        let Some(rec) = self.recorded.get(self.next) else {
            return Err(EffectDivergence::Inserted { position });
        };
        if rec.target != req.target {
            return Err(EffectDivergence::TargetDiverged {
                position,
                recorded: rec.target,
                asked: req.target,
            });
        }
        if rec.op != req.op {
            return Err(EffectDivergence::OpDiverged {
                position,
                recorded: rec.op,
                asked: req.op,
            });
        }
        let asked = req.payload_digest();
        if rec.payload != asked {
            return Err(EffectDivergence::PayloadDiverged {
                position,
                recorded: rec.payload,
                asked,
            });
        }
        Ok(rec)
    }
}

impl EffectPort for ShadowPort<'_> {
    fn submit(&mut self, req: EffectRequest) -> Result<Vec<u8>, EffectDivergence> {
        if let Some(d) = self.frontier {
            return Err(d);
        }
        match self.compare(&req) {
            Ok(rec) => {
                self.next = self.next.saturating_add(1);
                Ok(rec.result.clone())
            }
            Err(d) => {
                self.frontier = Some(d);
                Err(d)
            }
        }
    }
}

/// Records live submissions made through a host-supplied closure (the record pass).
pub struct RecordingPort<F: FnMut(&EffectRequest) -> Vec<u8>> {
    sink: F,
    records: Vec<EffectRecord>,
    next_seq: u64,
}

impl<F: FnMut(&EffectRequest) -> Vec<u8>> RecordingPort<F> {
    /// A recorder whose first record is `seq` 0.
    #[inline]
    pub fn new(sink: F) -> Self {
        Self::starting_at(0, sink)
    }

    /// A recorder whose first record is `seq` `first_seq` (a log continuation).
    #[inline]
    pub const fn starting_at(first_seq: u64, sink: F) -> Self {
        RecordingPort {
            sink,
            records: Vec::new(),
            next_seq: first_seq,
        }
    }

    /// The records so far.
    #[inline]
    #[must_use]
    pub fn records(&self) -> &[EffectRecord] {
        &self.records
    }

    /// The records, in submission order.
    #[inline]
    #[must_use]
    pub fn into_records(self) -> Vec<EffectRecord> {
        self.records
    }
}

impl<F: FnMut(&EffectRequest) -> Vec<u8>> EffectPort for RecordingPort<F> {
    fn submit(&mut self, req: EffectRequest) -> Result<Vec<u8>, EffectDivergence> {
        // The sequence space is checked before the sink runs: an effect the recording could
        // not hold is never submitted.
        let Some(after) = self.next_seq.checked_add(1) else {
            return Err(EffectDivergence::Inserted {
                position: self.next_seq,
            });
        };
        let result = (self.sink)(&req);
        self.records.push(EffectRecord {
            seq: self.next_seq,
            target: req.target,
            op: req.op,
            payload: req.payload_digest(),
            result: result.clone(),
        });
        self.next_seq = after;
        Ok(result)
    }
}

/// For tasks that declare no external effects: any submission is `Undeclared`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoEffects;

impl EffectPort for NoEffects {
    #[inline]
    fn submit(&mut self, req: EffectRequest) -> Result<Vec<u8>, EffectDivergence> {
        Err(EffectDivergence::Undeclared { target: req.target })
    }
}
