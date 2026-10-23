// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! External effect recording and shadow replay verification.

use core::fmt;

use crate::ident::Digest;

/// Domain tag under which an effect payload is digested.
pub const EFFECT_PAYLOAD_DOMAIN: &[u8] = b"aterm-aos-effect-payload-v1\0";

/// External effect proposed by a task step.
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
    /// Returns the payload digest stored in an [`EffectRecord`].
    #[inline]
    #[must_use]
    pub fn payload_digest(&self) -> Digest {
        Digest::of(EFFECT_PAYLOAD_DOMAIN, &[&self.payload])
    }
}

/// Recorded external effect request and its observed result.
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

/// Port to which a step submits its external effects.
pub trait EffectPort {
    /// Submits one request and returns the observed result or the divergence that refused it.
    fn submit(&mut self, req: EffectRequest) -> Result<Vec<u8>, EffectDivergence>;
}

/// Reason why a replay's external effects diverged from the recorded sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectDivergence {
    /// Request submitted past the end of the recording.
    Inserted {
        /// The request's position.
        position: u64,
    },
    /// Request target differed from the recorded target at `position`.
    TargetDiverged {
        /// The position.
        position: u64,
        /// The recorded target.
        recorded: u32,
        /// The target asked for.
        asked: u32,
    },
    /// Request operation differed from the recorded operation at `position`.
    OpDiverged {
        /// The position.
        position: u64,
        /// The recorded op.
        recorded: u16,
        /// The op asked for.
        asked: u16,
    },
    /// Request payload digest differed from the recorded digest at `position`.
    PayloadDiverged {
        /// The position.
        position: u64,
        /// The recorded payload digest.
        recorded: Digest,
        /// The asked payload's digest.
        asked: Digest,
    },
    /// Run ended with unconsumed recorded effects remaining at [`ShadowPort::finish`].
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

/// Shadow replay port that verifies each request against `recorded` before returning the recorded result.
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

    /// Returns the first sticky divergence, if any.
    #[inline]
    #[must_use]
    pub const fn frontier(&self) -> Option<&EffectDivergence> {
        self.frontier.as_ref()
    }

    /// Returns the number of requests that matched their recorded effects.
    #[inline]
    #[must_use]
    pub fn matched(&self) -> u64 {
        crate::ident::len_u64(self.next)
    }

    /// Finishes replay, returning the matched count or an [`EffectDivergence`].
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

/// Records live effect submissions executed through `sink`.
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

/// Rejects every effect submission with [`EffectDivergence::Undeclared`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoEffects;

impl EffectPort for NoEffects {
    #[inline]
    fn submit(&mut self, req: EffectRequest) -> Result<Vec<u8>, EffectDivergence> {
        Err(EffectDivergence::Undeclared { target: req.target })
    }
}
