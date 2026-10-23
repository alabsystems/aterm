// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The task ABI: a [`Schema`] (what the state, inputs and observations are and how they are
//! encoded) and a [`Code`] (one implementation of the step function over that schema).

use core::fmt;

use crate::effects::EffectPort;
use crate::ident::{ArtifactId, Digest};
use crate::wire::{Dec, Enc, WireFault};

/// The domain under which [`state_digest`] digests a state.
pub const STATE_DOMAIN: &[u8] = b"aterm-aos-state-v1\0";

/// While a boundary is paused, what an input does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PausedClass {
    /// It mutates task state the host cannot re-derive (a key, a reset): it waits in the bounded
    /// queue and is delivered once, in order, when the pause ends.
    Queue,
    /// It is a frame or query the task simply does not observe while paused: it is skipped, neither
    /// recorded nor delivered.
    Skip,
}

/// What a hosted task's state, inputs and observations are, and their canonical encodings.
pub trait Schema {
    /// The task's owned state: plain data, no references, handles or host pointers.
    type State: Clone;
    /// One input event (time enters here, as recorded offsets).
    type Input: Clone;
    /// What one step makes visible (frames, schedule answers), judged by a contract.
    type Observation: Clone + PartialEq + core::fmt::Debug;

    /// The schema's artifact (kind [`crate::ident::ArtifactKind::Schema`]).
    fn id(&self) -> &ArtifactId;
    /// Writes the state's canonical encoding.
    fn encode_state(&self, s: &Self::State, e: &mut Enc);
    /// Reads a state; must accept exactly what [`Schema::encode_state`] writes.
    fn decode_state(&self, d: &mut Dec<'_>) -> Result<Self::State, WireFault>;
    /// Writes an input's canonical encoding.
    fn encode_input(&self, i: &Self::Input, e: &mut Enc);
    /// Reads an input; must accept exactly what [`Schema::encode_input`] writes.
    fn decode_input(&self, d: &mut Dec<'_>) -> Result<Self::Input, WireFault>;
    /// Writes an observation's canonical encoding (folded into a run's observation chain).
    fn encode_observation(&self, o: &Self::Observation, e: &mut Enc);
    /// While paused: does this input queue (it mutates task state the host cannot re-derive) or is
    /// it skipped (a frame/query the task simply does not observe)?
    fn paused_class(&self, i: &Self::Input) -> PausedClass;
}

/// One implementation of a task's step function over schema `S`.
pub trait Code<S: Schema> {
    /// The code's artifact (kind [`crate::ident::ArtifactKind::Code`]).
    fn id(&self) -> &ArtifactId;

    /// The digest of the schema this code reads and writes.
    #[inline]
    fn schema_digest(&self, schema: &S) -> Digest {
        schema.id().digest
    }

    /// The candidate's entry invariants on a state it is asked to continue from.
    fn entry(&self, schema: &S, state: &S::State) -> Result<(), EntryRefusal>;

    /// One step.
    fn step(
        &self,
        schema: &S,
        state: &mut S::State,
        input: &S::Input,
        port: &mut dyn EffectPort,
    ) -> Result<S::Observation, StepRefusal>;
}

/// A state the code may not continue from: the law it breaks and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryRefusal {
    /// The entry invariant broken (a stable name, e.g. `toy.non-negative`).
    pub law: &'static str,
    /// The offending values.
    pub detail: String,
}

impl fmt::Display for EntryRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the state breaks the entry invariant `{}`: {}",
            self.law, self.detail
        )
    }
}

impl std::error::Error for EntryRefusal {}

/// An input the code refused (e.g. admission): the law and the offending values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepRefusal {
    /// The law the input breaks (a stable name).
    pub law: &'static str,
    /// The offending values.
    pub detail: String,
}

impl fmt::Display for StepRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the step refused its input under `{}`: {}",
            self.law, self.detail
        )
    }
}

impl std::error::Error for StepRefusal {}

/// The state's canonical encoding.
#[inline]
pub(crate) fn encode_state<S: Schema>(schema: &S, s: &S::State) -> Vec<u8> {
    let mut e = Enc::new();
    schema.encode_state(s, &mut e);
    e.finish()
}

/// The digest of an encoded state under a schema digest.
#[inline]
pub(crate) fn state_digest_of(schema_digest: &Digest, encoded: &[u8]) -> Digest {
    Digest::of(STATE_DOMAIN, &[&schema_digest.0, encoded])
}

/// The state's canonical digest: `Digest::of(b"aterm-aos-state-v1\0", [schema digest, encoded
/// state])`.
#[inline]
#[must_use]
pub fn state_digest<S: Schema>(schema: &S, s: &S::State) -> Digest {
    state_digest_of(&schema.id().digest, &encode_state(schema, s))
}
