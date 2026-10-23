// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Full-copy checkpoints of a hosted task's state, with their provenance.

use core::fmt;

use astream_wire::frame::{Frame, MAX_PAYLOAD_LEN, decode_frame_slice};

use crate::ident::{BranchId, Digest, Revision, TaskIdentity};
use crate::log::IdentityMismatch;
use crate::task::{Schema, encode_state, state_digest_of};
use crate::wire::{Dec, Enc, WireFault};

/// The body's magic: what tells a checkpoint frame from any other astream frame.
pub const CHECKPOINT_MAGIC: [u8; 8] = *b"AOSCKPT\0";

/// The body version this build writes and reads.
pub const CHECKPOINT_VERSION: u8 = 1;

/// The domain under which [`Checkpoint::digest`] is taken.
pub const CHECKPOINT_DOMAIN: &[u8] = b"aterm-aos-checkpoint-v1\0";

/// Where a checkpoint came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// The task identity the state ran under.
    pub identity: TaskIdentity,
    /// The log epoch that starts at this checkpoint.
    pub log_epoch: u64,
    /// The cursor of the next input.
    pub input_cursor: u64,
    /// The cursor of the next effect.
    pub effect_cursor: u64,
    /// The branch of the head it was taken from.
    pub branch: BranchId,
    /// The revision of the head it was taken from.
    pub revision: Revision,
    /// The digest of the checkpoint it descends from, if any.
    pub parent: Option<Digest>,
}

fn encode_provenance(e: &mut Enc, p: &Provenance) {
    e.identity(&p.identity);
    e.u64(p.log_epoch);
    e.u64(p.input_cursor);
    e.u64(p.effect_cursor);
    e.u64(p.branch.0);
    e.u64(p.revision.0);
    e.opt(p.parent.as_ref(), |e, d| e.digest(d));
}

fn decode_provenance(d: &mut Dec<'_>) -> Result<Provenance, WireFault> {
    Ok(Provenance {
        identity: d.identity()?,
        log_epoch: d.u64("provenance.log_epoch")?,
        input_cursor: d.u64("provenance.input_cursor")?,
        effect_cursor: d.u64("provenance.effect_cursor")?,
        branch: BranchId(d.u64("provenance.branch")?),
        revision: Revision(d.u64("provenance.revision")?),
        parent: d.opt("provenance.parent", |d| d.digest("provenance.parent"))?,
    })
}

/// Why bytes are not a checkpoint this build reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointFault {
    /// The astream frame is incomplete or damaged (its error, rendered).
    Frame(String),
    /// A body of another [`CHECKPOINT_VERSION`].
    Version(u8),
    /// A body that is not the canonical encoding of a checkpoint.
    Wire(WireFault),
    /// The stored state digest is not the digest of the stored state.
    StateDigest,
    /// The frame holds something other than an Aos checkpoint.
    NotACheckpoint,
    /// Encoding only: the body of `len` bytes exceeds the `limit` a frame carries.
    TooLarge {
        /// The body length.
        len: usize,
        /// The most a frame carries.
        limit: usize,
    },
}

impl fmt::Display for CheckpointFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointFault::Frame(s) => write!(f, "the checkpoint frame is unreadable: {s}"),
            CheckpointFault::Version(v) => {
                write!(
                    f,
                    "the checkpoint is of version {v}, which this build does not read"
                )
            }
            CheckpointFault::Wire(w) => write!(f, "the checkpoint body is not canonical: {w}"),
            CheckpointFault::StateDigest => {
                f.write_str("the stored state digest is not the digest of the stored state")
            }
            CheckpointFault::NotACheckpoint => {
                f.write_str("the frame holds something other than an Aos checkpoint")
            }
            CheckpointFault::TooLarge { len, limit } => write!(
                f,
                "the checkpoint body of {len} bytes exceeds the {limit} a frame carries"
            ),
        }
    }
}

impl std::error::Error for CheckpointFault {}

impl From<WireFault> for CheckpointFault {
    #[inline]
    fn from(w: WireFault) -> Self {
        CheckpointFault::Wire(w)
    }
}

/// Why a checkpoint's state is not handed to the task asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreRefusal {
    /// The checkpoint belongs to another task, schema or profile.
    Identity(IdentityMismatch),
    /// The stored state digest is not the digest of the stored state under the restoring schema's
    /// digest: the checkpoint was captured under another schema than the one its provenance names.
    StateDigest {
        /// The digest the checkpoint carries.
        stored: Digest,
        /// The digest of its state bytes under the restoring schema.
        derived: Digest,
    },
    /// The schema does not decode the stored state canonically.
    Wire(WireFault),
}

impl fmt::Display for RestoreRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreRefusal::Identity(m) => {
                write!(f, "the checkpoint belongs to another identity: {m}")
            }
            RestoreRefusal::StateDigest { stored, derived } => write!(
                f,
                "the stored state digest {} is not {}, the state's digest under the restoring schema",
                stored.short(),
                derived.short()
            ),
            RestoreRefusal::Wire(w) => write!(f, "the stored state does not decode: {w}"),
        }
    }
}

impl std::error::Error for RestoreRefusal {}

/// Immutable after capture: private fields, getters only, no in-place mutation API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    provenance: Provenance,
    state: Vec<u8>,
    state_digest: Digest,
}

impl Checkpoint {
    /// Captures `state` (as its canonical encoding) with its provenance.
    #[must_use]
    pub fn capture<S: Schema>(schema: &S, state: &S::State, provenance: Provenance) -> Checkpoint {
        let state = encode_state(schema, state);
        let state_digest = state_digest_of(&schema.id().digest, &state);
        Checkpoint {
            provenance,
            state,
            state_digest,
        }
    }

    /// Where it came from.
    #[inline]
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// The state's digest (= [`crate::task::state_digest`] of the captured state).
    #[inline]
    #[must_use]
    pub const fn state_digest(&self) -> Digest {
        self.state_digest
    }

    /// `Digest::of(b"aterm-aos-checkpoint-v1\0", [encoded provenance, state digest])`.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut e = Enc::new();
        encode_provenance(&mut e, &self.provenance);
        Digest::of(CHECKPOINT_DOMAIN, &[e.as_bytes(), &self.state_digest.0])
    }

    /// The checkpoint's bytes.
    pub fn encode(&self) -> Result<Vec<u8>, CheckpointFault> {
        let mut e = Enc::with_capacity(self.state.len().saturating_add(256));
        e.raw(&CHECKPOINT_MAGIC);
        e.u8(CHECKPOINT_VERSION);
        encode_provenance(&mut e, &self.provenance);
        e.digest(&self.state_digest);
        e.bytes(&self.state);
        let body = e.finish();
        let len = body.len();
        Frame::new(body)
            .encode()
            .map_err(|_| CheckpointFault::TooLarge {
                len,
                limit: MAX_PAYLOAD_LEN,
            })
    }

    /// Reads a checkpoint.
    pub fn decode(bytes: &[u8]) -> Result<Checkpoint, CheckpointFault> {
        let (body, consumed) = match decode_frame_slice(bytes) {
            Ok(Some(found)) => found,
            Ok(None) => {
                return Err(CheckpointFault::Frame(format!(
                    "the frame is incomplete: {} bytes are not a whole frame",
                    bytes.len()
                )));
            }
            Err(e) => return Err(CheckpointFault::Frame(e.to_string())),
        };
        if consumed < bytes.len() {
            return Err(CheckpointFault::Wire(WireFault::Trailing {
                extra: bytes.len() - consumed,
            }));
        }
        let mut d = Dec::new(body);
        let magic = d
            .raw("checkpoint.magic", CHECKPOINT_MAGIC.len())
            .map_err(|_| CheckpointFault::NotACheckpoint)?;
        if magic != CHECKPOINT_MAGIC {
            return Err(CheckpointFault::NotACheckpoint);
        }
        let version = d.u8("checkpoint.version")?;
        if version != CHECKPOINT_VERSION {
            return Err(CheckpointFault::Version(version));
        }
        let provenance = decode_provenance(&mut d)?;
        let state_digest = d.digest("checkpoint.state_digest")?;
        let state = d.bytes("checkpoint.state")?.to_vec();
        d.finish()?;
        if state_digest_of(&provenance.identity.schema.digest, &state) != state_digest {
            return Err(CheckpointFault::StateDigest);
        }
        Ok(Checkpoint {
            provenance,
            state,
            state_digest,
        })
    }

    /// Validates task/schema/profile against `expect` (the code may differ: applying other code is
    /// a separately recorded transition), re-derives the state digest under `schema`'s digest, and
    /// returns the decoded state.
    ///
    /// # Errors
    ///
    /// [`RestoreRefusal::Identity`] (task, schema or profile, in that order),
    /// [`RestoreRefusal::StateDigest`], or [`RestoreRefusal::Wire`] (the state does not decode, or
    /// does not re-encode to its own bytes).
    pub fn restore<S: Schema>(
        &self,
        schema: &S,
        expect: &TaskIdentity,
    ) -> Result<S::State, RestoreRefusal> {
        let p = &self.provenance.identity;
        if p.task != expect.task {
            return Err(RestoreRefusal::Identity(IdentityMismatch::Task));
        }
        if p.schema != expect.schema || schema.id() != &expect.schema {
            return Err(RestoreRefusal::Identity(IdentityMismatch::Schema));
        }
        if p.profile != expect.profile {
            return Err(RestoreRefusal::Identity(IdentityMismatch::Profile));
        }
        let derived = state_digest_of(&schema.id().digest, &self.state);
        if derived != self.state_digest {
            return Err(RestoreRefusal::StateDigest {
                stored: self.state_digest,
                derived,
            });
        }
        let mut d = Dec::new(&self.state);
        let state = schema.decode_state(&mut d).map_err(RestoreRefusal::Wire)?;
        d.finish().map_err(RestoreRefusal::Wire)?;
        if encode_state(schema, &state) != self.state {
            return Err(RestoreRefusal::Wire(WireFault::NonCanonical {
                field: "state",
            }));
        }
        Ok(state)
    }
}
