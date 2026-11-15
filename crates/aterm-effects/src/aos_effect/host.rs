// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The generic host: an engine drawn directly, or recorded as an Aos task behind a publish
//! boundary, with the verbs `aos` drives it by.

use core::fmt;
use std::time::Duration;

use aterm_aos::Digest;
use aterm_aos::checkpoint::CheckpointFault;
use aterm_aos::log::LogFault;
use aterm_aos::publish::{Admit, Boundary, Commit, Head, Prepared, PublishRefusal, RETIRED_CAP};
use aterm_aos::replay::{chain_link, empty_chain};
use aterm_aos::task::{Code, Schema, StepRefusal, state_digest};
use aterm_aos::wire::Enc;
use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::{EffectCode, EffectSchema, EffectTrace, HostedEngine, Segment, identity_of, offset_of};

/// The live observation chain since the current ancestor checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiveChain {
    /// The log epoch and first cursor it folds from.
    pub(crate) anchor: (u64, u64),
    pub(crate) chain: Digest,
    pub(crate) len: u64,
}

/// The last delivered tick's frame header, the quads it appended, and what it was drawn at.
pub struct Held<E: HostedEngine> {
    pub(crate) frame: E::Shown,
    pub(crate) out: Vec<GlowQuad>,
    pub(crate) under: Vec<GlowQuad>,
    pub(crate) at: E::Presentation,
}

impl<E: HostedEngine> Held<E> {
    /// The frame header it draws again.
    #[inline]
    #[must_use]
    pub const fn frame(&self) -> &E::Shown {
        &self.frame
    }

    /// The additive quads it draws again.
    #[inline]
    #[must_use]
    pub fn out(&self) -> &[GlowQuad] {
        &self.out
    }

    /// The under-ink quads it draws again.
    #[inline]
    #[must_use]
    pub fn under(&self) -> &[GlowQuad] {
        &self.under
    }
}

impl<E: HostedEngine> fmt::Debug for Held<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Held")
            .field("frame", &self.frame)
            .field("out", &self.out.len())
            .field("under", &self.under.len())
            .field("at", &self.at)
            .finish()
    }
}

/// Refills `v` with `from`, keeping (or growing) its capacity, and dropping it for none.
#[inline]
pub(crate) fn refill(v: &mut Vec<GlowQuad>, from: &[GlowQuad]) {
    if from.is_empty() {
        // Empty presentations release the old allocation rather than retaining it.
        drop(std::mem::take(v));
        return;
    }
    if v.capacity() < from.len() {
        let room = from.len().max(v.capacity().saturating_mul(2));
        drop(std::mem::take(v));
        v.reserve_exact(room);
    } else {
        v.clear();
    }
    v.extend_from_slice(from);
}

/// An engine recorded behind its publish boundary.
pub struct Attached<E: HostedEngine> {
    pub(crate) boundary: Boundary<EffectSchema<E>, EffectCode<E>, E::Contract>,
    pub(crate) build: String,
    pub(crate) chain: LiveChain,
    pub(crate) held: Option<Held<E>>,
    /// How many live inputs were recorded at the logical time instead of their own.
    pub(crate) clamped: u64,
    /// The latest time among the inputs queued in the current pause (within the horizon).
    pub(crate) queued_at: Option<i128>,
    /// The one encoder every live observation is encoded into before its chain link.
    pub(crate) scratch: Enc,
    /// What the engine keeps per attachment.
    pub(crate) live: E::Live,
    /// While a typed key waits for its program's echo ([`Hosted::aos_hold`]): the instant the host
    /// wakes by. No input reaches the boundary meanwhile.
    pub(crate) awaiting_echo: Option<Instant>,
}

impl<E: HostedEngine> Attached<E> {
    /// The recording epoch.
    #[inline]
    pub(crate) const fn epoch(&self) -> Instant {
        self.boundary.schema().epoch
    }

    /// The live state.
    #[inline]
    pub(crate) const fn state(&self) -> &E::State {
        &self.boundary.pair().state
    }

    /// What answers for the live code.
    #[inline]
    pub(crate) const fn code(&self) -> E::Code {
        self.boundary.pair().code.code
    }

    /// The live pair's input cursor.
    #[inline]
    pub(crate) const fn cursor(&self) -> u64 {
        self.boundary.pair().head.input_cursor
    }

    /// The boundary.
    #[inline]
    #[must_use]
    pub const fn boundary(&self) -> &Boundary<EffectSchema<E>, EffectCode<E>, E::Contract> {
        &self.boundary
    }

    /// The last delivered tick, held.
    #[inline]
    #[must_use]
    pub const fn held(&self) -> Option<&Held<E>> {
        self.held.as_ref()
    }

    /// The logical time of a live call the host stamped `now`, and whether it was clamped.
    #[inline]
    pub(crate) fn logical(&self, now: Instant) -> (i128, bool) {
        let at = offset_of(self.epoch(), now);
        let mut floor = self.last_admitted();
        if self.boundary.is_paused()
            && let Some(queued) = self.queued_at
        {
            floor = floor.max(queued);
        }
        if at < floor {
            (floor, true)
        } else {
            (at, false)
        }
    }

    /// The latest instant the live state admitted (signed ns from the epoch, never before it).
    #[inline]
    pub(crate) fn last_admitted(&self) -> i128 {
        E::last_time(self.state(), self.epoch()).unwrap_or(0).max(0)
    }

    /// What `input` draws under the live code on a copy of the live state: nothing reaches the
    /// boundary, the log or the chain. A held engine draws its still frame with it, an input at
    /// [`Self::last_admitted`].
    #[inline]
    pub(crate) fn still(
        &self,
        input: &E::Input,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> E::Applied {
        let mut state = self.state().clone();
        E::apply(self.code(), &mut state, self.epoch(), input, out, under)
    }

    #[inline]
    pub(crate) fn fold(&mut self, cursor: u64, obs: &E::Frame) {
        self.link(cursor, |schema, e| schema.encode_observation(obs, e));
    }

    #[inline]
    pub(crate) fn link(&mut self, cursor: u64, encode: impl FnOnce(&EffectSchema<E>, &mut Enc)) {
        let Some(log) = self.boundary.log() else {
            return;
        };
        let anchor = (log.header().log_epoch, log.header().first_cursor);
        if self.chain.anchor != anchor {
            self.chain = LiveChain {
                anchor,
                chain: empty_chain(),
                len: 0,
            };
        }
        if cursor < anchor.1 {
            return;
        }
        self.scratch.clear();
        encode(self.boundary.schema(), &mut self.scratch);
        self.chain.chain = chain_link(&self.chain.chain, cursor, self.scratch.as_bytes());
        self.chain.len = self.chain.len.saturating_add(1);
    }

    /// The live observation chain since the ancestor checkpoint.
    #[inline]
    #[must_use]
    pub fn live_chain(&self) -> Digest {
        match self.boundary.log() {
            Some(log)
                if self.chain.anchor == (log.header().log_epoch, log.header().first_cursor) =>
            {
                self.chain.chain
            }
            _ => empty_chain(),
        }
    }

    #[inline]
    pub(crate) fn fold_delivered(
        &mut self,
        first: u64,
        delivered: &[Result<E::Frame, StepRefusal>],
    ) {
        let mut cursor = first;
        let mut di = 0;
        while di < delivered.len() {
            if let Ok(obs) = &delivered[di] {
                self.fold(cursor, obs);
                cursor = cursor.saturating_add(1);
            }
            di += 1;
        }
    }

    /// One live input through the boundary: admitted, then (unless it queued, was skipped or
    /// was rejected) applied under the live code into `out` and `under`, chained and held.
    pub(crate) fn deliver(
        &mut self,
        ev: &E::Input,
        moved: bool,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Option<E::Applied> {
        let first = self.cursor();
        let admitted = self.boundary.admit(ev);
        if moved && admitted != Admit::Skipped {
            self.clamped = self.clamped.saturating_add(1);
        }
        match admitted {
            Admit::Run => {}
            Admit::Queued => {
                // A queued time past the horizon is refused on delivery; it raises no floor.
                if let Some(at) = E::time_of(ev)
                    && at <= E::horizon_ns()
                {
                    self.queued_at = Some(match self.queued_at {
                        Some(q) => q.max(at),
                        None => at,
                    });
                }
                return None;
            }
            Admit::Skipped => return None,
            Admit::Rejected { .. } => {
                let delivered = self.boundary.take_delivered();
                self.fold_delivered(first, &delivered);
                return None;
            }
        }
        let (epoch, code) = (self.epoch(), self.code());
        let (out_from, under_from) = (out.len(), under.len());
        let (_, state) = self.boundary.pair_mut();
        let applied = E::apply(code, state, epoch, ev, out, under);
        self.boundary.advance();
        let drawn = out.get(out_from..).unwrap_or(&[]);
        let drawn_under = under.get(under_from..).unwrap_or(&[]);
        self.link(first, |_, e| {
            E::encode_applied(&applied, epoch, drawn, drawn_under, e);
        });
        if let Some((frame, at)) = E::held_of(ev, &applied) {
            let (mut held_out, mut held_under) = match self.held.take() {
                Some(h) => (h.out, h.under),
                None => (Vec::new(), Vec::new()),
            };
            refill(&mut held_out, drawn);
            refill(&mut held_under, drawn_under);
            if drawn.is_empty() && drawn_under.is_empty() {
                self.scratch = Enc::new();
            }
            self.held = Some(Held {
                frame,
                out: held_out,
                under: held_under,
                at,
            });
        }
        let (code, epoch) = (self.code(), self.epoch());
        let state = &self.boundary.pair().state;
        E::noted(&mut self.live, code, state, ev, &applied, epoch);
        Some(applied)
    }

    /// The held tick a paused `ev` (a skipped tick) draws again, if it shows.
    #[inline]
    #[must_use]
    pub(crate) fn shown(&self, ev: &E::Input) -> Option<&Held<E>> {
        self.held.as_ref().filter(|h| E::shows(&h.at, ev))
    }

    /// Whether a reset now changes nothing: the paused queue ends in one, or the state rests.
    #[inline]
    pub(crate) fn reset_changes_nothing(&self) -> bool {
        if self.boundary.is_paused() {
            match self.boundary.last_queued() {
                Some(i) if E::is_reset(i) => true,
                Some(_) => false,
                None => E::at_rest(self.state(), self.epoch()),
            }
        } else {
            E::at_rest(self.state(), self.epoch())
        }
    }
}

/// How the host holds its engine.
pub enum HostMode<E: HostedEngine> {
    /// Drawn directly: nothing is recorded.
    Direct(E::Engine),
    /// Recorded behind a publish boundary.
    Hosted(Box<Attached<E>>),
}

/// Why the host stopped recording by itself: live time passed the hosted horizon.
pub const PAST_HORIZON: &str = "past the hosted horizon";

/// Why a key stopped the recording: a program reads it raw (or the tty's mode is unreadable),
/// no prompt of aterm's own integrated shell is open for it, and its own echo was not proven in
/// place, so it may be a password. The host stops for it by itself when a key or a reset arrives
/// while a key is held.
pub const RAW_KEY: &str = "raw key";

/// An effect engine as the host holds it: drawn directly, or recorded as an Aos task.
pub struct Hosted<E: HostedEngine> {
    pub(crate) mode: HostMode<E>,
    /// Retired segments of the recordings the host dropped (a detach or a re-attach), oldest first, at most [`RETIRED_CAP`].
    pub(crate) left: Vec<Segment<E>>,
    /// Why the host last stopped recording by itself, until [`Self::aos_take_detached`] takes it.
    pub(crate) detached: Option<&'static str>,
    /// Whether the live engine took a key no recording logged since it last started from rest:
    /// the next new recording starts it from rest, so its checkpoint holds no such key's trace.
    pub(crate) tainted: bool,
}

impl<E: HostedEngine> Default for Hosted<E> {
    #[inline]
    fn default() -> Self {
        Hosted {
            mode: HostMode::Direct(E::Engine::default()),
            left: Vec::new(),
            detached: None,
            tainted: false,
        }
    }
}

impl<E: HostedEngine> fmt::Debug for Hosted<E>
where
    E::Engine: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mode {
            HostMode::Direct(g) => f
                .debug_tuple(&format!("{}::Direct", E::SHORT_NAME))
                .field(g)
                .finish(),
            HostMode::Hosted(h) => f
                .debug_struct(&format!("{}::Hosted", E::SHORT_NAME))
                .field("boundary", &h.boundary)
                .finish_non_exhaustive(),
        }
    }
}

impl<E: HostedEngine> Hosted<E> {
    /// The engine drawn directly, as `engine`.
    #[inline]
    #[must_use]
    pub fn direct(engine: E::Engine) -> Self {
        Hosted {
            mode: HostMode::Direct(engine),
            left: Vec::new(),
            detached: None,
            tainted: false,
        }
    }

    /// The engine while it is drawn directly.
    #[inline]
    #[must_use]
    pub const fn direct_engine(&self) -> Option<&E::Engine> {
        match &self.mode {
            HostMode::Direct(g) => Some(g),
            HostMode::Hosted(_) => None,
        }
    }

    /// The engine while it is drawn directly, mutably.
    #[inline]
    #[must_use]
    pub const fn direct_engine_mut(&mut self) -> Option<&mut E::Engine> {
        match &mut self.mode {
            HostMode::Direct(g) => Some(g),
            HostMode::Hosted(_) => None,
        }
    }

    /// The recording, while there is one.
    #[inline]
    #[must_use]
    pub fn attached(&self) -> Option<&Attached<E>> {
        match &self.mode {
            HostMode::Direct(_) => None,
            HostMode::Hosted(h) => Some(h),
        }
    }

    /// A running attach whose live time at `now` is past the hosted horizon can admit no timed
    /// input again (logical time never goes back, and every such input is refused `Horizon`).
    #[inline]
    pub(crate) fn expire_past_horizon(&mut self, now: Instant) {
        if let HostMode::Hosted(h) = &self.mode
            && !h.boundary.is_paused()
            && h.logical(now).0 > E::horizon_ns()
        {
            self.detach_by_itself(PAST_HORIZON);
        }
    }

    /// Stops recording for the reason `why`, keeping the recording's retired segments for the watcher.
    #[inline]
    pub(crate) fn detach_by_itself(&mut self, why: &'static str) {
        self.aos_detach();
        self.detached = Some(why);
    }

    /// Moves the recording's undrained retired segments into the ones kept for [`Self::aos_take_retired`], the oldest dropped past [`RETIRED_CAP`].
    #[inline]
    fn keep_retired(&mut self) {
        let retired = self.aos_take_retired();
        self.left = retired;
        let over = self.left.len().saturating_sub(RETIRED_CAP);
        self.left.drain(..over);
    }

    /// Why the host stopped recording by itself since the last call, taken.
    #[inline]
    pub const fn aos_take_detached(&mut self) -> Option<&'static str> {
        self.detached.take()
    }

    /// Starts hosting under the prebuilt `code`: one log epoch holds the engine's
    /// [`HostedEngine::HOST_MAX_EVENTS`] inputs, a pause queues at most [`HostedEngine::QUEUE_CAP`].
    #[inline]
    pub fn aos_attach(&mut self, epoch: Instant, code: E::Prebuilt, build: &str, log_epoch: u64) {
        self.attach_with(
            epoch,
            code,
            build,
            log_epoch,
            E::QUEUE_CAP,
            E::HOST_MAX_EVENTS,
        );
    }

    #[inline]
    pub(crate) fn attach_with(
        &mut self,
        epoch: Instant,
        code: E::Prebuilt,
        build: &str,
        log_epoch: u64,
        queue_cap: usize,
        max_events: usize,
    ) {
        self.attach_code(
            epoch,
            E::code_of(code),
            build,
            log_epoch,
            queue_cap,
            max_events,
        );
    }

    /// Starts (or, on the same epoch, code and build, with no key held and no taint, re-anchors) a
    /// recording under `code`; a tainted engine starts from rest.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Attach",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    pub(crate) fn attach_code(
        &mut self,
        epoch: Instant,
        code: E::Code,
        build: &str,
        log_epoch: u64,
        queue_cap: usize,
        max_events: usize,
    ) {
        if let HostMode::Hosted(h) = &mut self.mode
            && h.epoch() == epoch
            && h.code() == code
            && h.build == build
            && h.awaiting_echo.is_none()
            && !self.tainted
        {
            let first = h.cursor();
            if h.boundary.attach(log_epoch, max_events).is_ok() {
                let delivered = h.boundary.take_delivered();
                h.fold_delivered(first, &delivered);
            }
            return;
        }
        let mut engine = self.take_engine();
        if core::mem::take(&mut self.tainted) {
            engine = E::Engine::default();
        }
        let compiled = EffectCode::<E>::compiled(code, build);
        let identity = identity_of(&compiled);
        let mut boundary = Boundary::new(
            EffectSchema::<E>::new(epoch),
            identity,
            compiled,
            E::contract(),
            E::state_of(engine),
            queue_cap,
        );
        if boundary.attach(log_epoch, max_events).is_err() {
            self.mode = HostMode::Direct(E::engine_of(boundary.pair().state.clone()));
            return;
        }
        let chain = LiveChain {
            anchor: match boundary.log() {
                Some(l) => (l.header().log_epoch, l.header().first_cursor),
                None => (log_epoch, 0),
            },
            chain: empty_chain(),
            len: 0,
        };
        self.mode = HostMode::Hosted(Box::new(Attached {
            boundary,
            build: build.to_owned(),
            chain,
            held: None,
            clamped: 0,
            queued_at: None,
            scratch: Enc::new(),
            live: E::Live::default(),
            awaiting_echo: None,
        }));
    }

    /// The engine out of the host, any recording dropped with its undrained retired segments kept
    /// for the watcher; a hold dropped taints the engine, whose held keys the caller hands it
    /// unrecorded.
    #[inline]
    pub(crate) fn take_engine(&mut self) -> E::Engine {
        self.keep_retired();
        match core::mem::replace(&mut self.mode, HostMode::Direct(E::Engine::default())) {
            HostMode::Direct(g) => g,
            HostMode::Hosted(mut h) => {
                if h.awaiting_echo.is_some() {
                    self.tainted = true;
                }
                h.boundary.detach();
                E::engine_of(h.boundary.pair().state.clone())
            }
        }
    }

    /// Stops hosting; the engine draws on from its live state. A hold it drops taints the engine.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Detach",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    #[inline]
    pub fn aos_detach(&mut self) {
        let engine = self.take_engine();
        self.mode = HostMode::Direct(engine);
    }

    /// Holds the recording while a typed key waits for its program's echo: until a release or a
    /// detach no input reaches the boundary (a tick draws the engine as it stood at the latest
    /// admitted instant, [`Attached::still`]; the wake is `deadline`). Unattached or paused it does
    /// nothing.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Hold",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    #[inline]
    pub fn aos_hold(&mut self, deadline: Instant) {
        if let HostMode::Hosted(h) = &mut self.mode
            && !h.boundary.is_paused()
        {
            h.awaiting_echo = Some(deadline);
        }
    }

    /// Whether a typed key holds the recording ([`Self::aos_hold`]).
    #[inline]
    #[must_use]
    pub const fn aos_holding(&self) -> bool {
        matches!(&self.mode, HostMode::Hosted(h) if h.awaiting_echo.is_some())
    }

    /// Notes that the live engine takes a key the recording may not hold: the next new recording
    /// starts it from rest.
    #[inline]
    pub const fn taint(&mut self) {
        self.tainted = true;
    }

    /// Whether the live engine took a key no recording logged since it last started from rest.
    #[inline]
    #[must_use]
    pub const fn aos_tainted(&self) -> bool {
        self.tainted
    }

    /// Pauses at this step boundary, after [`HostedEngine::before_pause`] records what it records.
    /// # Errors
    /// [`PublishRefusal::NotAttached`], [`PublishRefusal::AlreadyPaused`], or
    /// [`PublishRefusal::MidStep`] while a key is held (a step the host has not taken); nothing is
    /// recorded then.
    #[inline]
    pub fn aos_pause(
        &mut self,
        now: Instant,
        frame_interval: Duration,
    ) -> Result<Head, PublishRefusal> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PublishRefusal::NotAttached);
        };
        if h.boundary.is_paused() {
            return Err(PublishRefusal::AlreadyPaused);
        }
        if h.awaiting_echo.is_some() {
            return Err(PublishRefusal::MidStep);
        }
        E::before_pause(h, now, frame_interval);
        h.queued_at = None;
        h.boundary.pause()
    }

    /// Prepares `candidate` against the paused head (shadow replay of the live code and the
    /// candidate from the ancestor checkpoint, judged by the contract) and commits it.
    /// # Errors
    /// Any [`PublishRefusal`] `prepare` or `commit` returns (`NotAttached`, `NotPaused`,
    /// `CandidateFailed`, …).
    #[inline]
    pub fn aos_repair(
        &mut self,
        candidate: E::Prebuilt,
    ) -> Result<Commit<EffectSchema<E>>, PublishRefusal> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PublishRefusal::NotAttached);
        };
        let prebuilt = EffectCode::<E>::prebuilt(candidate, &h.build);
        let prepared = h.boundary.prepare(prebuilt)?;
        let first = h.cursor();
        let commit = h.boundary.commit(prepared)?;
        h.fold_delivered(first, &commit.delivered);
        Ok(commit)
    }

    /// A copy of the paused boundary and `candidate`, for [`RepairJob::prepare`] on another thread.
    /// # Errors
    /// [`PublishRefusal::NotAttached`] or [`PublishRefusal::NotPaused`].
    #[inline]
    pub fn aos_repair_job(&self, candidate: E::Prebuilt) -> Result<RepairJob<E>, PublishRefusal> {
        let HostMode::Hosted(h) = &self.mode else {
            return Err(PublishRefusal::NotAttached);
        };
        if !h.boundary.is_paused() {
            return Err(PublishRefusal::NotPaused);
        }
        Ok(RepairJob {
            boundary: Box::new(h.boundary.clone()),
            candidate: EffectCode::<E>::prebuilt(candidate, &h.build),
        })
    }

    /// Commits a prepare made on a copy of this boundary, with every check `commit` makes, and folds what the queue returned.
    /// # Errors
    /// The prepare's refusal, then [`PublishRefusal::NotAttached`] or any refusal `commit` returns (`NotPaused`, `StaleHead`, `WitnessMismatch`, …).
    #[inline]
    pub fn aos_commit(
        &mut self,
        prepared: PreparedRepair<E>,
    ) -> Result<Commit<EffectSchema<E>>, PublishRefusal> {
        let prepared = prepared.prepared?;
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PublishRefusal::NotAttached);
        };
        let first = h.cursor();
        let commit = h.boundary.commit(prepared)?;
        h.fold_delivered(first, &commit.delivered);
        Ok(commit)
    }

    /// How many retired log epochs wait for [`Self::aos_take_retired`].
    #[inline]
    #[must_use]
    pub fn aos_retired_waiting(&self) -> usize {
        self.left.len()
            + match &self.mode {
                HostMode::Direct(_) => 0,
                HostMode::Hosted(h) => h.boundary.retired_len(),
            }
    }

    /// Takes the retired log epochs, oldest first, for the online watcher ([`Segment::judge`]), with those of the recordings the host dropped kept.
    #[inline]
    pub fn aos_take_retired(&mut self) -> Vec<Segment<E>> {
        let mut out = core::mem::take(&mut self.left);
        if let HostMode::Hosted(h) = &mut self.mode {
            let schema = h.boundary.schema().clone();
            let drained = h.boundary.drain_retired();
            out.reserve(drained.len());
            for retired in drained {
                out.push(Segment::new(schema.clone(), retired));
            }
        }
        out
    }

    /// Rejects the pending update: the old code resumes over the queue.
    #[inline]
    pub fn aos_resume(&mut self) -> Vec<Result<E::Frame, StepRefusal>> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Vec::new();
        };
        let first = h.cursor();
        let delivered = h.boundary.reject();
        h.fold_delivered(first, &delivered);
        delivered
    }

    /// Whether the engine is recorded.
    #[inline]
    #[must_use]
    pub const fn aos_attached(&self) -> bool {
        matches!(self.mode, HostMode::Hosted(_))
    }

    /// Whether the recorded engine is paused.
    #[inline]
    #[must_use]
    pub const fn aos_paused(&self) -> bool {
        matches!(&self.mode, HostMode::Hosted(h) if h.boundary.is_paused())
    }

    /// The head the recorded engine is paused at, if it is.
    #[inline]
    #[must_use]
    pub const fn aos_paused_head(&self) -> Option<&Head> {
        match &self.mode {
            HostMode::Direct(_) => None,
            HostMode::Hosted(h) => h.boundary.paused_head(),
        }
    }

    /// The prebuilt code recording the engine (the shipped one unattached), or `None` for a code no verb names.
    #[inline]
    #[must_use]
    pub fn aos_prebuilt(&self) -> Option<E::Prebuilt> {
        match &self.mode {
            HostMode::Direct(_) => E::ALL.first().copied(),
            HostMode::Hosted(h) => E::prebuilt_of(h.code()),
        }
    }

    /// The ancestor checkpoint and the log since it.
    /// # Errors
    /// [`TraceRefusal::NotAttached`], or an encoder's `TooLarge`.
    #[inline]
    pub fn aos_trace(&self) -> Result<EffectTrace<E>, TraceRefusal> {
        let HostMode::Hosted(h) = &self.mode else {
            return Err(TraceRefusal::NotAttached);
        };
        let b = &h.boundary;
        let (Some(checkpoint), Some(log)) = (b.checkpoint(), b.log()) else {
            return Err(TraceRefusal::NotAttached);
        };
        Ok(EffectTrace::new(
            b.identity().clone(),
            checkpoint.encode().map_err(TraceRefusal::Checkpoint)?,
            log.encode(b.schema()).map_err(TraceRefusal::Log)?,
            state_digest(b.schema(), h.state()),
            h.live_chain(),
        ))
    }

    /// What every hosted engine's status shares, as plain data.
    #[inline]
    #[must_use]
    pub fn aos_effect_status(&self) -> EffectAosStatus {
        let HostMode::Hosted(h) = &self.mode else {
            return EffectAosStatus {
                task: E::SHORT_NAME,
                code: E::ALL.first().map_or("none", |p| E::prebuilt_name(*p)),
                ..EffectAosStatus::default()
            };
        };
        let b = &h.boundary;
        let (log_epoch, log_len, broken) = match b.log() {
            Some(l) => (l.header().log_epoch, l.len(), l.broken()),
            None => (0, 0, None),
        };
        EffectAosStatus {
            task: E::SHORT_NAME,
            attached: true,
            code: E::code_name(h.code()),
            code_digest: Some(b.pair().code.id().digest.short()),
            build: Some(h.build.clone()),
            revision: b.pair().head.revision.0,
            branch: b.pair().head.branch.0,
            paused: b.is_paused(),
            queued: b.queued(),
            queue_cap: b.queue_cap(),
            log_epoch,
            log_len,
            input_cursor: b.pair().head.input_cursor,
            broken,
            checkpoint: b.checkpoint().map(|c| c.digest().short()),
            state_digest: Some(state_digest(b.schema(), h.state()).short()),
            observation_chain: Some(h.live_chain().short()),
            clock: E::last_time(h.state(), h.epoch()),
            held: h.held.is_some(),
            stats: b.stats(),
            clamped: h.clamped,
        }
    }
}

/// What every hosted engine's status shares ([`Hosted::aos_effect_status`]), as plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectAosStatus {
    /// The task's short name.
    pub task: &'static str,
    pub attached: bool,
    /// The running code's short name.
    pub code: &'static str,
    /// Its artifact digest, short.
    pub code_digest: Option<String>,
    pub build: Option<String>,
    pub revision: u64,
    pub branch: u64,
    pub paused: bool,
    pub queued: usize,
    pub queue_cap: usize,
    pub log_epoch: u64,
    pub log_len: usize,
    pub input_cursor: u64,
    /// Why the log is broken, if it is (a repair across it refuses).
    pub broken: Option<&'static str>,
    /// The ancestor checkpoint's digest, short.
    pub checkpoint: Option<String>,
    pub state_digest: Option<String>,
    /// The live observation chain since the ancestor, short.
    pub observation_chain: Option<String>,
    /// The latest admitted input's time.
    pub clock: Option<i128>,
    /// Whether a delivered tick is held for a pause to draw again.
    pub held: bool,
    pub stats: aterm_aos::publish::Stats,
    /// How many live inputs were recorded at the task's logical time instead of their own earlier one.
    pub clamped: u64,
}

/// A paused boundary, copied, with the candidate to prepare against it away from the host's main thread.
pub struct RepairJob<E: HostedEngine> {
    boundary: Box<Boundary<EffectSchema<E>, EffectCode<E>, E::Contract>>,
    candidate: EffectCode<E>,
}

impl<E: HostedEngine> fmt::Debug for RepairJob<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RepairJob")
            .field("boundary", &self.boundary)
            .field("candidate", &self.candidate.id)
            .finish()
    }
}

impl<E: HostedEngine> RepairJob<E> {
    /// Shadow-replays the copy's live code and the candidate from the ancestor checkpoint ([`Boundary::prepare`]).
    #[inline]
    #[must_use]
    pub fn prepare(self) -> PreparedRepair<E> {
        let mut boundary = self.boundary;
        let before = boundary.stats().replays;
        let prepared = boundary.prepare(self.candidate);
        PreparedRepair {
            replays: boundary.stats().replays.saturating_sub(before),
            prepared,
        }
    }
}

/// What [`RepairJob::prepare`] made, for [`Hosted::aos_commit`].
pub struct PreparedRepair<E: HostedEngine> {
    prepared: Result<Prepared<EffectSchema<E>, EffectCode<E>>, PublishRefusal>,
    replays: u64,
}

impl<E: HostedEngine> fmt::Debug for PreparedRepair<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedRepair")
            .field("prepared", &self.prepared)
            .field("replays", &self.replays)
            .finish()
    }
}

impl<E: HostedEngine> PreparedRepair<E> {
    /// How many replays the prepare ran (on the copy, never on the live boundary).
    #[inline]
    #[must_use]
    pub const fn replays(&self) -> u64 {
        self.replays
    }

    /// The prepare's refusal, if it refused.
    #[inline]
    #[must_use]
    pub const fn refusal(&self) -> Option<&PublishRefusal> {
        match &self.prepared {
            Ok(_) => None,
            Err(r) => Some(r),
        }
    }
}

/// Why [`Hosted::aos_trace`] exported nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceRefusal {
    /// The engine is not attached: there is no checkpoint or log.
    NotAttached,
    /// The checkpoint does not encode.
    Checkpoint(CheckpointFault),
    /// The log does not encode.
    Log(LogFault),
}

impl fmt::Display for TraceRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceRefusal::NotAttached => {
                f.write_str("the momentum glow is not attached: there is no trace")
            }
            TraceRefusal::Checkpoint(c) => {
                write!(f, "the ancestor checkpoint does not encode: {c}")
            }
            TraceRefusal::Log(l) => write!(f, "the log does not encode: {l}"),
        }
    }
}

impl std::error::Error for TraceRefusal {}
