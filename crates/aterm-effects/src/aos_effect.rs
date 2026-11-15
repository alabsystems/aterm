// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Generic hosted effects (design §14.4.6): a [`HostedEngine`] is one effect engine's task ABI,
//! [`EffectSchema`] and [`EffectCode`] adapt it to the substrate's [`Schema`] and [`Code`], and
//! [`Hosted`] is the host that draws with it directly or records it as an Aos task.

use core::fmt;
use core::marker::PhantomData;
use std::time::Duration;

use aterm_aos::ArtifactId;
use aterm_aos::contract::Contract;
use aterm_aos::effects::EffectPort;
use aterm_aos::task::{Code, EntryRefusal, PausedClass, Schema, StepRefusal};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_render::GlowQuad;
use aterm_time::Instant;

mod host;
mod trace;
mod watch;

pub use host::{
    Attached, EffectAosStatus, Held, HostMode, Hosted, PAST_HORIZON, PreparedRepair, RAW_KEY,
    RepairJob, TraceRefusal,
};
pub use trace::{EffectTrace, ReadTrace, TraceCheck, TraceFault, TraceReplay, identity_of};
pub use watch::{Segment, SegmentJudgement};

/// `t − epoch` in signed nanoseconds.
#[inline]
pub(crate) fn offset_of(epoch: Instant, t: Instant) -> i128 {
    if let Some(after) = t.checked_duration_since(epoch) {
        let n = after.as_nanos();
        if n <= i128::MAX as u128 {
            n as i128
        } else {
            i128::MAX
        }
    } else if let Some(before) = epoch.checked_duration_since(t) {
        let n = before.as_nanos();
        if n <= i128::MAX as u128 {
            -(n as i128)
        } else {
            i128::MIN
        }
    } else {
        i128::MIN
    }
}

/// `epoch + at` with checked instant arithmetic.
#[inline]
pub(crate) fn instant_of(epoch: Instant, at: i128) -> Option<Instant> {
    let magnitude = at.unsigned_abs();
    let secs = u64::try_from(magnitude / 1_000_000_000).ok()?;
    let nanos = u32::try_from(magnitude % 1_000_000_000).ok()?;
    let span = Duration::new(secs, nanos);
    if at >= 0 {
        epoch.checked_add(span)
    } else {
        epoch.checked_sub(span)
    }
}

/// One effect engine as an Aos task: its state, inputs and observations with their canonical
/// encodings, its prebuilt codes, its contract, and how the host draws and holds its frames.
///
/// A live call is admitted and applied by [`HostedEngine::apply`], which draws into the caller's
/// two quad streams; a replay applies the same function into fresh streams and observes the
/// result with [`HostedEngine::observe`], so what the live host chains and what a replay folds
/// are one encoding ([`HostedEngine::encode_applied`] must write exactly what
/// [`HostedEngine::encode_frame`] writes for the observation [`HostedEngine::observe`] makes).
pub trait HostedEngine: Sized + 'static {
    /// What the host draws with while nothing is recorded (its default at rest).
    type Engine: Default;
    /// The task's owned state: the engine and what admission needs (plain data).
    type State: Clone;
    /// The engine's semantic snapshot: the state as plain values.
    type Snapshot: Clone + PartialEq + fmt::Debug;
    /// One input: one host call, its time a recorded offset from the epoch.
    type Input: Clone + PartialEq + fmt::Debug;
    /// What one step makes visible (the observation a contract judges).
    type Frame: Clone + PartialEq + fmt::Debug;
    /// The limits inputs are admitted under.
    type Profile: 'static;
    /// What a live call returns to the host's caller.
    type Applied: Clone;
    /// What answers for a code: a prebuilt implementation (or momentum's schedule plan).
    type Code: Copy + Eq + fmt::Debug;
    /// A prebuilt implementation a verb names.
    type Prebuilt: Copy + Eq + fmt::Debug + 'static;
    /// The task's contract.
    type Contract: Contract<EffectSchema<Self>> + Clone + fmt::Debug;
    /// A held tick's frame header, which a paused host draws again while it shows.
    type Shown: Copy + Default + PartialEq + fmt::Debug;
    /// What a held tick was drawn at (cursor cell, geometry, the config that decides its look).
    type Presentation: Copy + PartialEq + fmt::Debug;
    /// How many inputs of each kind a log holds.
    type Counts: Copy + Default + PartialEq + Eq + fmt::Debug;
    /// What the host keeps per attachment beside the boundary.
    type Live: Default;

    /// The task artifact's name.
    const TASK_NAME: &'static str;
    /// The short name `aos … task=` takes.
    const SHORT_NAME: &'static str;
    /// How many queued inputs a pause holds.
    const QUEUE_CAP: usize;
    /// How many inputs one host log epoch holds before the recorder re-anchors.
    const HOST_MAX_EVENTS: usize;
    /// The most events a reader of a log accepts.
    const MAX_EVENTS: usize;
    /// The bytes that open the task's trace file.
    const TRACE_MAGIC: [u8; 8];
    /// The domain of the digest that closes the task's trace file.
    const TRACE_DOMAIN: &'static [u8];
    /// Every prebuilt implementation in this build, the shipped one first.
    const ALL: &'static [Self::Prebuilt];

    /// The admission profile.
    fn profile() -> &'static Self::Profile;
    /// The hosted horizon in signed nanoseconds.
    fn horizon_ns() -> i128;
    /// The task artifact.
    fn task_artifact() -> ArtifactId;
    /// The schema artifact.
    fn schema_artifact() -> ArtifactId;
    /// The profile artifact.
    fn profile_artifact() -> ArtifactId;
    /// The contract.
    fn contract() -> Self::Contract;

    /// The state over `engine`, having seen no input since the epoch.
    fn state_of(engine: Self::Engine) -> Self::State;
    /// The engine out of `state`.
    fn engine_of(state: Self::State) -> Self::Engine;
    /// The state's snapshot over `epoch`, or `None` when an instant does not fit.
    fn snapshot(state: &Self::State, epoch: Instant) -> Option<Self::Snapshot>;
    /// Writes the state's canonical encoding over `epoch`.
    fn encode_state(state: &Self::State, epoch: Instant, e: &mut Enc);
    /// Reads a state over `epoch`; accepts exactly what [`Self::encode_state`] writes.
    /// # Errors
    /// A field that does not decode or a state the engine cannot reach.
    fn decode_state(epoch: Instant, d: &mut Dec<'_>) -> Result<Self::State, WireFault>;
    /// Writes an input's canonical encoding.
    fn encode_input(i: &Self::Input, e: &mut Enc);
    /// Reads an input; accepts exactly what [`Self::encode_input`] writes.
    /// # Errors
    /// A field that does not decode.
    fn decode_input(d: &mut Dec<'_>) -> Result<Self::Input, WireFault>;
    /// Writes an observation's canonical encoding.
    fn encode_frame(o: &Self::Frame, e: &mut Enc);
    /// Whether `i` queues or is skipped while the boundary is paused.
    fn paused_class(i: &Self::Input) -> PausedClass;
    /// An input's time, if it has one.
    fn time_of(i: &Self::Input) -> Option<i128>;
    /// Whether `i` puts the engine back to rest.
    fn is_reset(i: &Self::Input) -> bool;
    /// The latest time the state has admitted (signed ns from `epoch`), if any.
    fn last_time(state: &Self::State, epoch: Instant) -> Option<i128>;
    /// Whether the state is at rest.
    fn at_rest(state: &Self::State, epoch: Instant) -> bool;
    /// The counts of each kind over `inputs`.
    fn counts(inputs: &[Self::Input]) -> Self::Counts;
    /// How many keys the state took that were logged with no proven echo, under the person's
    /// grant, since its recording began (an engine that logs no such key: none).
    #[inline]
    fn unseen(_state: &Self::State) -> u32 {
        0
    }
    /// How many of the inputs `counts` counted are such keys.
    #[inline]
    fn unseen_logged(_counts: &Self::Counts) -> usize {
        0
    }

    /// What answers for the prebuilt `p`.
    fn code_of(p: Self::Prebuilt) -> Self::Code;
    /// The prebuilt implementation `c` is, if it is one.
    fn prebuilt_of(c: Self::Code) -> Option<Self::Prebuilt>;
    /// The short name a status line shows for `c`.
    fn code_name(c: Self::Code) -> &'static str;
    /// The short name a verb takes for `p`.
    fn prebuilt_name(p: Self::Prebuilt) -> &'static str;
    /// The prebuilt implementation a short or artifact name names.
    fn prebuilt_named(name: &str) -> Option<Self::Prebuilt>;
    /// `c`'s code artifact as compiled into `build`.
    fn code_artifact(c: Self::Code, build: &str) -> ArtifactId;
    /// `c`'s entry invariants on a state it is asked to continue from.
    /// # Errors
    /// The entry law the state breaks.
    fn entry(c: Self::Code, state: &Self::State, epoch: Instant) -> Result<(), EntryRefusal>;

    /// One input, admitted then applied under `c`, its quads appended to `out` and `under`.
    fn apply(
        c: Self::Code,
        state: &mut Self::State,
        epoch: Instant,
        input: &Self::Input,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Self::Applied;
    /// The observation `applied` makes, with the quads it appended.
    fn observe(
        applied: &Self::Applied,
        epoch: Instant,
        out: Vec<GlowQuad>,
        under: Vec<GlowQuad>,
    ) -> Self::Frame;
    /// Writes [`Self::observe`]'s observation of `applied` and its quads without making it.
    fn encode_applied(
        applied: &Self::Applied,
        epoch: Instant,
        out: &[GlowQuad],
        under: &[GlowQuad],
        e: &mut Enc,
    );
    /// For a tick: the frame header a paused host draws again and what it was drawn at.
    fn held_of(
        input: &Self::Input,
        applied: &Self::Applied,
    ) -> Option<(Self::Shown, Self::Presentation)>;
    /// Whether a paused `input` (a skipped tick) shows the frame held at `at`.
    fn shows(at: &Self::Presentation, input: &Self::Input) -> bool;

    /// After a live step: notes what the host keeps per attachment.
    #[inline]
    fn noted(
        _live: &mut Self::Live,
        _c: Self::Code,
        _state: &Self::State,
        _input: &Self::Input,
        _applied: &Self::Applied,
        _epoch: Instant,
    ) {
    }

    /// What a pause records first, at `now` with the lane's `frame_interval`.
    #[inline]
    fn before_pause(_a: &mut Attached<Self>, _now: Instant, _frame_interval: Duration) {}
}

/// A hosted engine's task ABI over its recording epoch.
pub struct EffectSchema<E: HostedEngine> {
    pub(crate) id: ArtifactId,
    pub(crate) epoch: Instant,
    _e: PhantomData<fn() -> E>,
}

impl<E: HostedEngine> EffectSchema<E> {
    /// The schema over the recording epoch `epoch`.
    #[inline]
    #[must_use]
    pub fn new(epoch: Instant) -> Self {
        EffectSchema {
            id: E::schema_artifact(),
            epoch,
            _e: PhantomData,
        }
    }

    /// The recording epoch.
    #[inline]
    #[must_use]
    pub const fn epoch(&self) -> Instant {
        self.epoch
    }
}

impl<E: HostedEngine> Clone for EffectSchema<E> {
    #[inline]
    fn clone(&self) -> Self {
        EffectSchema {
            id: self.id.clone(),
            epoch: self.epoch,
            _e: PhantomData,
        }
    }
}

impl<E: HostedEngine> fmt::Debug for EffectSchema<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct(E::SHORT_NAME)
            .field("id", &self.id)
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl<E: HostedEngine> Schema for EffectSchema<E> {
    type State = E::State;
    type Input = E::Input;
    type Observation = E::Frame;

    #[inline]
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    #[inline]
    fn encode_state(&self, s: &E::State, e: &mut Enc) {
        E::encode_state(s, self.epoch, e);
    }

    #[inline]
    fn decode_state(&self, d: &mut Dec<'_>) -> Result<E::State, WireFault> {
        E::decode_state(self.epoch, d)
    }

    #[inline]
    fn encode_input(&self, i: &E::Input, e: &mut Enc) {
        E::encode_input(i, e);
    }

    #[inline]
    fn decode_input(&self, d: &mut Dec<'_>) -> Result<E::Input, WireFault> {
        E::decode_input(d)
    }

    #[inline]
    fn encode_observation(&self, o: &E::Frame, e: &mut Enc) {
        E::encode_frame(o, e);
    }

    #[inline]
    fn paused_class(&self, i: &E::Input) -> PausedClass {
        E::paused_class(i)
    }
}

/// A hosted engine's code bound to its artifact, which names the build it was compiled into.
pub struct EffectCode<E: HostedEngine> {
    pub(crate) code: E::Code,
    pub(crate) id: ArtifactId,
}

impl<E: HostedEngine> EffectCode<E> {
    /// The code `code` names, as compiled into the build `build`.
    #[inline]
    #[must_use]
    pub(crate) fn compiled(code: E::Code, build: &str) -> Self {
        EffectCode {
            code,
            id: E::code_artifact(code, build),
        }
    }

    /// The prebuilt implementation `p`, as compiled into the build `build`.
    #[inline]
    #[must_use]
    pub fn prebuilt(p: E::Prebuilt, build: &str) -> Self {
        Self::compiled(E::code_of(p), build)
    }

    /// What answers for this code.
    #[inline]
    #[must_use]
    pub const fn answer(&self) -> E::Code {
        self.code
    }

    /// The prebuilt implementation it is, if it is one.
    #[inline]
    #[must_use]
    pub fn prebuilt_code(&self) -> Option<E::Prebuilt> {
        E::prebuilt_of(self.code)
    }
}

impl<E: HostedEngine> Clone for EffectCode<E> {
    #[inline]
    fn clone(&self) -> Self {
        EffectCode {
            code: self.code,
            id: self.id.clone(),
        }
    }
}

impl<E: HostedEngine> PartialEq for EffectCode<E> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code && self.id == other.id
    }
}

impl<E: HostedEngine> Eq for EffectCode<E> {}

impl<E: HostedEngine> fmt::Debug for EffectCode<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectCode")
            .field("code", &self.code)
            .field("id", &self.id)
            .finish()
    }
}

impl<E: HostedEngine> Code<EffectSchema<E>> for EffectCode<E> {
    #[inline]
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    #[inline]
    fn entry(&self, schema: &EffectSchema<E>, state: &E::State) -> Result<(), EntryRefusal> {
        E::entry(self.code, state, schema.epoch)
    }

    #[inline]
    fn step(
        &self,
        schema: &EffectSchema<E>,
        state: &mut E::State,
        input: &E::Input,
        _port: &mut dyn EffectPort,
    ) -> Result<E::Frame, StepRefusal> {
        let (mut out, mut under) = (Vec::new(), Vec::new());
        let applied = E::apply(self.code, state, schema.epoch, input, &mut out, &mut under);
        Ok(E::observe(&applied, schema.epoch, out, under))
    }
}
