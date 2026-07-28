// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The **Observation Kernel** (RFC "The Reactive Surface", layer L0).
//!
//! The terminal is already an event-sourced surface: every program byte folds
//! into the grid and bumps exactly one write-only logical clock,
//! [`Terminal::content_seq`](super::Terminal::content_seq). This module turns
//! *"observe this surface until condition C holds"* into a single, first-class,
//! **event-driven** primitive evaluated at the one seam where every mutation has
//! already landed — [`Terminal::post_process`] — and **latched** there so a
//! transiently-true condition can never be lost to a coalesced wake. Live deltas,
//! idle-quiescence, and semantic row matching all become *the same mechanism
//! armed with a different predicate* — never a poll loop, never a text-hash
//! scrape. Three of the four predicates (`SeqAdvanced`, `IdleFor`, `RowMatches`)
//! are OSC-133-independent — the property that makes turn-detection work for an
//! alt-screen agent TUI like Claude; `BlockComplete` is the deliberate exception,
//! exposing shell-integration block state (OSC 133/633) as a fourth predicate.
//!
//! ## One list, one path
//!
//! Every armed observer is a [`Watcher`] in ONE list, distinguished only by its
//! [`WatcherSpec`]. [`WatcherSet::observe`] evaluates all four predicate kinds in
//! one match at the seam; `IdleFor` additionally fires from [`WatcherSet::expire`]
//! at a host-supplied instant. The kernel carries no vocabulary — the regex
//! behind [`WatcherSpec::RowMatches`] is an opaque [`RowMatch`] built one crate up
//! in `aterm-observe`, so the kernel never constructs or names a regex and
//! `aterm-core` takes no **direct** `regex` dependency (it remains a transitive
//! dep via `aterm-search`'s `regex` feature; the purity test checks the direct
//! production deps).
//!
//! ## Correctness properties (model-checked and/or conformance-bound)
//!
//! 1. **No silent loss.** A predicate that holds at *any* processed batch latches
//!    at that batch, not on the consumer's later wake. Modeled abstractly by
//!    `watcher_latch_model` and behaviorally conformance-tested by
//!    `conformance_observe.rs`.
//! 2. **Deterministic idle.** `IdleFor` latches at the *exact computed deadline*
//!    (`activity_at + dur`), never the observation instant — so a live wake and a
//!    lazy replay tick latch the identical [`Satisfaction`]. This is verified by
//!    the unit + `conformance_observe` determinism tests. (`idle_deadline_model`
//!    proves a related but distinct property: the host arms the *single earliest*
//!    of all pending deadlines, via [`WatcherSet::next_deadline`].)
//!
//! ## Replay-safe by construction (IdleFor-under-replay)
//!
//! The kernel is **ephemeral, observation-only state**: it reads `content_seq`
//! and (read-only) grid rows and updates its own watcher list, but it **never
//! mutates the surface** and is **never part of a [`TerminalCheckpoint`]** — so it
//! cannot perturb the `replay_from_checkpoint_matches_live_engine` property or the
//! astream-oracle cross-check. The activity instant is the already-deterministic
//! [`process_now`](super::TransientState::process_now) reconstructed from recorded
//! `Ticks` on replay; the kernel **never reads the wall clock** (the `bell.rs`
//! caller-injects-now discipline).

use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;

/// A handle to one armed watcher, unique within a [`WatcherSet`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WatchId(pub u64);

/// An opaque, pre-compiled row matcher. The concrete implementation (regex) lives
/// one crate up in `aterm-observe` (layer L0.5); the core stores it behind this
/// trait so the kernel never names or constructs a regex — `aterm-core` takes no
/// **direct** `regex` dependency (RFC R2 purity, checked by
/// `regex_is_not_in_aterm_core_production_deps`). The core evaluates a match; it
/// cannot construct one from a pattern string.
pub trait RowMatch: Send + Sync + std::fmt::Debug {
    /// Does `row` (one visible row's text) satisfy this matcher?
    fn matches(&self, row: &str) -> bool;
}

/// An inclusive range of **visible** row indices to match against, or every
/// visible row. Constructed in `aterm-observe`; carried opaquely by the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowRange {
    /// Every visible row (`0..rows`).
    All,
    /// The inclusive visible-row span `start..=end` (clamped to the grid).
    Span {
        /// First visible row index (inclusive).
        start: usize,
        /// Last visible row index (inclusive).
        end: usize,
    },
}

impl RowRange {
    /// Whether visible row `idx` is in range.
    #[must_use]
    fn contains(self, idx: usize) -> bool {
        match self {
            RowRange::All => true,
            RowRange::Span { start, end } => idx >= start && idx <= end,
        }
    }
}

/// The condition a watcher waits for.
#[derive(Clone, Debug)]
pub enum WatcherSpec {
    /// Latch once `content_seq()` exceeds `after`. Monotonic, trivially loss-free.
    SeqAdvanced {
        /// Latch once `content_seq()` exceeds this value.
        after: u64,
    },
    /// Latch after `dur` of no content mutation (quiescence / turn-completion).
    /// Latches at the exact deadline `activity_at + dur`, independent of when it
    /// is observed — the determinism property.
    IdleFor {
        /// Latch after this much wall-time with no content mutation.
        dur: Duration,
    },
    /// Latch when the visible surface shows a completed/prompt-ready shell block.
    BlockComplete,
    /// Latch when any visible row in `rows` satisfies `matcher` (the semantic
    /// predicate; the `matcher` is built in `aterm-observe`, regex out of core).
    RowMatches {
        /// The opaque pre-compiled matcher (regex lives in `aterm-observe`).
        matcher: Arc<dyn RowMatch>,
        /// Which visible rows to scan.
        rows: RowRange,
    },
}

impl WatcherSpec {
    #[inline]
    fn is_row(&self) -> bool {
        matches!(self, WatcherSpec::RowMatches { .. })
    }

    /// Whether the predicate must be evaluated ONCE AT ARM, not only on the next
    /// batch — i.e. whether it is **level**- rather than edge-triggered.
    ///
    /// Both kinds here are statements about the surface as it ALREADY IS, so an
    /// arm-only-then-wait evaluation answers "no" while the answer is plainly
    /// "yes": an already-matching row for `RowMatches`, and an ALREADY-ADVANCED
    /// `content_seq` for `SeqAdvanced`. The latter is the exact shape of a
    /// turn-based agent's dirty check — "did anything change since the seq I
    /// recorded last turn?" — which must be answerable without waiting for the
    /// *next* unrelated batch to arrive (and, on a quiet session, forever).
    ///
    /// `IdleFor` and `BlockComplete` are deliberately excluded: idle is
    /// arm-relative by design (see [`WatcherSet::arm`]), and `BlockComplete`
    /// latching at arm would make `await block` fire on the PREVIOUS block.
    #[inline]
    fn needs_arm_eval(&self) -> bool {
        matches!(
            self,
            WatcherSpec::RowMatches { .. } | WatcherSpec::SeqAdvanced { .. }
        )
    }
}

/// A latched satisfaction. For [`WatcherSpec::IdleFor`], `at` is the exact
/// **deadline** (not the observation instant), which is what makes live and
/// replay latch identically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Satisfaction {
    /// The `content_seq` in force when the predicate became true.
    pub seq: u64,
    /// The instant the predicate became true: the processed-batch instant for
    /// `SeqAdvanced`/`BlockComplete`/`RowMatches`, the computed deadline for
    /// `IdleFor`.
    pub at: Instant,
}

/// The read-only surface a watcher evaluates against (RFC L3 boundary). The local
/// engine ([`Terminal`](super::Terminal)) implements it. A remote **astream-folded**
/// surface (`aterm-net`, L3) could implement the SAME trait — but per the L3 design
/// it is NOT required to *drive* a remote aterm: predicates run on the authoritative
/// host (the remote's own `WatcherSet`), and `aterm_agent::RelayClient` relays the
/// `await` verbs there, so remote == local **without any astream type entering
/// `aterm-core`**. The fold is only worth building for an OFFLINE record/replay
/// observer. Object-safe (owned returns) so `&dyn SurfaceSource` works for that case.
pub trait SurfaceSource {
    /// The monotonic content clock (`content_seq`).
    fn content_seq(&self) -> u64;
    /// Whether the newest shell-integration block is complete/prompt-ready.
    fn newest_block_complete(&self) -> bool;
    /// The number of visible rows.
    fn rows(&self) -> usize;
    /// The text of visible row `idx` (owned, for object safety), or `None`.
    fn row_text(&self, idx: usize) -> Option<String>;
}

/// The first visible row of `source` in `range` that `matcher` accepts — the
/// surface-agnostic core of `RowMatches`, runnable against a LOCAL engine or a
/// REMOTE astream-folded surface (both `impl SurfaceSource`).
#[must_use]
pub fn first_matching_row(
    source: &dyn SurfaceSource,
    matcher: &dyn RowMatch,
    range: RowRange,
) -> Option<usize> {
    (0..source.rows())
        .find(|&i| range.contains(i) && source.row_text(i).is_some_and(|t| matcher.matches(&t)))
}

/// The activity clock: the last `content_seq` seen to advance and the injected
/// instant at which it advanced. Never reads the wall clock.
#[derive(Clone, Copy, Debug, Default)]
struct ActivityClock {
    last_seq: u64,
    last_at: Option<Instant>,
    /// Which GRID `last_seq` was read from. `content_seq` is a **per-grid**
    /// counter, not a session clock: entering the alt screen installs a fresh
    /// grid whose `content_gen` restarts at 1, and leaving restores the main
    /// grid's stale-but-higher value. Comparing across a swap is comparing
    /// incomparable numbers, so the buffer identity travels with the reading and
    /// a flip is treated as a RESYNC rather than a comparison.
    ///
    /// Every other `content_seq` consumer already keys on this compound
    /// (`subscribe.rs`'s GAP-on-flip, `control_query`'s poll cache,
    /// `search_index`); the observation kernel was the one that did not.
    alt: bool,
}

/// One armed watcher — the single watcher type, distinguished only by `spec`.
#[derive(Clone)]
struct Watcher {
    id: WatchId,
    spec: WatcherSpec,
    /// For [`WatcherSpec::IdleFor`]: the current fire deadline (`activity_at +
    /// dur`), recomputed on each content advance. `None` for the other specs.
    deadline: Option<Instant>,
    /// `Some` once the predicate has held; sticky while armed.
    latched: Option<Satisfaction>,
    /// A freshly-armed `RowMatches` watcher is scanned once even without a content
    /// advance, so an ALREADY-matching row latches at arm. Cleared after the first
    /// `observe`.
    fresh: bool,
    /// Which GRID this watcher was armed against. `WatcherSpec::SeqAdvanced`'s
    /// `after` is a per-grid `content_gen`, so it is only comparable against a
    /// reading from the SAME buffer. Held privately here rather than in the
    /// public spec so the enum's shape is unchanged.
    ///
    /// A buffer swap is itself an observable surface change, so crossing one
    /// LATCHES rather than starving: without this, `await seq <main-n>` never
    /// fires while a TUI is on the alt screen (the counter restarted at 1), and
    /// on 1049-exit the restored high-water counter fires it with no content
    /// change at all.
    armed_alt: bool,
}

/// A bounded set of armed watchers plus the activity clock. Lives in
/// [`Terminal`](super::Terminal) as **ephemeral, observation-only** state: never
/// checkpointed, never mutates the surface.
#[derive(Clone)]
pub struct WatcherSet {
    watchers: Vec<Watcher>,
    clock: ActivityClock,
    next_id: u64,
    cap: usize,
}

/// Default capacity — bounds adversarial arming.
const DEFAULT_CAP: usize = 256;

impl Default for WatcherSet {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAP)
    }
}

impl WatcherSet {
    /// A set bounded to `cap` concurrently-armed watchers.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            watchers: Vec::new(),
            clock: ActivityClock::default(),
            next_id: 0,
            cap: cap.max(1),
        }
    }

    /// `true` iff at least one watcher is armed (the producer's `Wake::Output`
    /// fast-path gate, beside `subscribers.any()`).
    #[must_use]
    #[inline]
    pub fn has_armed(&self) -> bool {
        !self.watchers.is_empty()
    }

    /// `true` iff at least one armed watcher has latched.
    #[must_use]
    pub fn any_latched(&self) -> bool {
        self.watchers.iter().any(|w| w.latched.is_some())
    }

    /// The last `content_seq` the kernel saw advance — the dirty-row gate reads
    /// this to skip the row-text scan on quiescent frames.
    #[must_use]
    pub fn seen_seq(&self) -> u64 {
        self.clock.last_seq
    }

    /// Which GRID [`seen_seq`](Self::seen_seq) was read from. The dirty-row gate
    /// compares it so a 1049 swap counts as activity instead of being read as
    /// "the counter went backwards, nothing happened".
    #[must_use]
    pub fn seen_alt(&self) -> bool {
        self.clock.alt
    }

    /// Whether a row scan would do work this batch: some un-latched `RowMatches`
    /// watcher is either fresh (just armed) or content advanced. The Terminal glue
    /// collects row text only when this holds.
    #[must_use]
    pub fn wants_row_scan(&self, advanced: bool) -> bool {
        self.watchers
            .iter()
            .any(|w| w.latched.is_none() && w.spec.is_row() && (advanced || w.fresh))
    }

    /// Arm a watcher, returning its handle, or `None` (fail-closed) at capacity.
    /// `now` is the injected arming instant; it seeds the activity baseline so an
    /// `IdleFor` armed against an already-quiet surface still has a deadline.
    #[must_use]
    pub fn arm(&mut self, spec: WatcherSpec, now: Instant) -> Option<WatchId> {
        if self.watchers.len() >= self.cap {
            return None;
        }
        if self.clock.last_at.is_none() {
            self.clock.last_at = Some(now);
        }
        let id = WatchId(self.next_id);
        self.next_id += 1;
        // ARM-RELATIVE idle baseline: "idle for `dur`" is measured from arm, so
        // arming against an already-quiescent surface waits the full `dur` (and
        // resets on later activity) rather than firing on stale pre-arm idleness.
        let deadline = match &spec {
            WatcherSpec::IdleFor { dur } => Some(now + *dur),
            _ => None,
        };
        let fresh = spec.is_row();
        self.watchers.push(Watcher {
            id,
            spec,
            deadline,
            latched: None,
            fresh,
            // The clock's grid identity IS the arming grid: `Terminal::watch`
            // seeds it (`seed_seq_in`) immediately before arming.
            armed_alt: self.clock.alt,
        });
        Some(id)
    }

    /// Remove a watcher (its observer went away). Idempotent.
    pub fn disarm(&mut self, id: WatchId) {
        self.watchers.retain(|w| w.id != id);
    }

    /// Non-blocking: has `id` latched? `None` if pending or unknown.
    #[must_use]
    pub fn poll(&self, id: WatchId) -> Option<Satisfaction> {
        self.watchers
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.latched)
    }

    /// Seed the activity baseline so the first `observe` after arming does not
    /// read a phantom advance (the clock's `last_seq` defaults to 0). Monotone —
    /// never regresses a baseline a prior advance already set.
    pub fn seed_seq(&mut self, seq: u64) {
        if self.clock.last_seq < seq {
            self.clock.last_seq = seq;
        }
    }

    /// [`seed_seq`](Self::seed_seq) with the ACTIVE-GRID identity. On a grid
    /// change the baseline is SET, not raised — the monotone rule is only sound
    /// WITHIN one buffer, and applying it across a swap is what pins the clock
    /// at the main grid's high-water mark and starves every predicate for the
    /// whole alt-screen lifetime.
    pub fn seed_seq_in(&mut self, seq: u64, alt: bool) {
        if alt != self.clock.alt {
            self.clock.alt = alt;
            self.clock.last_seq = seq;
            return;
        }
        self.seed_seq(seq);
    }

    /// `true` iff an un-latched `BlockComplete` watcher is armed — gates the
    /// O(blocks) shell-integration walk in `observe_at` so it runs only when a
    /// `BlockComplete` predicate actually needs it.
    #[must_use]
    pub fn has_block_complete(&self) -> bool {
        self.watchers
            .iter()
            .any(|w| w.latched.is_none() && matches!(w.spec, WatcherSpec::BlockComplete))
    }

    /// The soonest pending `IdleFor` deadline. The L1 `await`/`ready` verb that
    /// armed it bounds its `Subscription::wait` park by this instant, so the kernel
    /// fires the idle predicate exactly on time without a GUI-loop timer. `None`
    /// when no un-latched idle watcher is armed.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.watchers
            .iter()
            .filter(|w| w.latched.is_none())
            .filter_map(|w| w.deadline)
            .min()
    }

    /// **The seam call** — run from `post_process` after every batch (and once at
    /// arm for a level-triggered spec: a fresh `RowMatches` or `SeqAdvanced`),
    /// with `now == transient.process_now`
    /// (injected, never read here). Stamps activity if `content_seq` advanced and
    /// latches any predicate that holds, evaluating all four kinds in ONE pass.
    /// Surface-read-only: `rows[idx]` supplies visible-row text for `RowMatches`
    /// (the caller gates the costly collection on `wants_row_scan` and passes the
    /// rows by reference, so a matched row is read — never re-cloned — here).
    /// Returns `true` if anything latched.
    pub fn observe(
        &mut self,
        content_seq: u64,
        newest_block_complete: bool,
        now: Instant,
        rows: &[Option<String>],
    ) -> bool {
        self.observe_in(content_seq, false, newest_block_complete, now, rows)
    }

    /// [`observe`](Self::observe) with the ACTIVE-GRID identity supplied — the
    /// form the engine calls. A change of `alt` means the counter came from a
    /// different grid, so it is a **resync**, never a comparison: activity is
    /// stamped and the baseline is SET (not monotonically raised, which is what
    /// would otherwise pin it at the main grid's high-water mark for the whole
    /// alt-screen lifetime and starve every predicate).
    pub fn observe_in(
        &mut self,
        content_seq: u64,
        alt: bool,
        newest_block_complete: bool,
        now: Instant,
        rows: &[Option<String>],
    ) -> bool {
        let flipped = alt != self.clock.alt;
        let advanced = flipped || content_seq > self.clock.last_seq;
        if advanced {
            self.clock.last_seq = content_seq;
            self.clock.alt = alt;
            self.clock.last_at = Some(now);
        }
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            // Decide the latch / deadline update with the spec borrow, then apply
            // the field writes AFTER the match (disjoint-borrow safe).
            let mut new_latch: Option<Satisfaction> = None;
            let mut new_deadline: Option<Instant> = None;
            match &w.spec {
                WatcherSpec::SeqAdvanced { after } => {
                    // `after` is a per-grid counter. Only compare it against a
                    // reading from the SAME grid; crossing a buffer swap is a
                    // real surface change, so it latches on the identity change
                    // rather than on an incomparable number.
                    if alt != w.armed_alt || content_seq > *after {
                        new_latch = Some(Satisfaction {
                            seq: content_seq,
                            at: now,
                        });
                    }
                }
                WatcherSpec::IdleFor { dur } => {
                    if advanced {
                        new_deadline = Some(now + *dur);
                    }
                }
                WatcherSpec::BlockComplete => {
                    if newest_block_complete {
                        new_latch = Some(Satisfaction {
                            seq: content_seq,
                            at: now,
                        });
                    }
                }
                WatcherSpec::RowMatches {
                    matcher,
                    rows: range,
                } => {
                    // Dirty-row gate: scan only on advance or a fresh arm. Reads
                    // the pre-collected row text by reference — no re-allocation.
                    if advanced || w.fresh {
                        for (idx, cell) in rows.iter().enumerate() {
                            if range.contains(idx) {
                                if let Some(t) = cell {
                                    if matcher.matches(t) {
                                        new_latch = Some(Satisfaction {
                                            seq: content_seq,
                                            at: now,
                                        });
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            w.fresh = false;
            if let Some(d) = new_deadline {
                w.deadline = Some(d);
            }
            if let Some(s) = new_latch {
                w.latched = Some(s);
                latched_any = true;
            }
        }
        latched_any
    }

    /// **The idle-fire call** — run by the host when its armed `WaitUntil` wake
    /// reaches `now` (and once at a replay target). Latches every un-latched
    /// `IdleFor` whose deadline has passed, recording `at = deadline` (NOT `now`)
    /// so the latched value is independent of *when* the host woke. Returns `true`
    /// if anything latched.
    pub fn expire(&mut self, now: Instant) -> bool {
        let last_seq = self.clock.last_seq;
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            if matches!(w.spec, WatcherSpec::IdleFor { .. }) {
                if let Some(deadline) = w.deadline {
                    if now >= deadline {
                        w.latched = Some(Satisfaction {
                            seq: last_seq,
                            at: deadline, // <-- deadline, not `now`: deterministic
                        });
                        latched_any = true;
                    }
                }
            }
        }
        latched_any
    }
}

impl super::Terminal {
    /// Run the Observation Kernel for the batch just processed. Called from
    /// [`process_at`](super::Terminal::process_at) immediately after
    /// `post_process` (and from [`watch`](Self::watch) at arm for a fresh row
    /// watcher), with the injected pipeline clock — never read here.
    /// Surface-read-only: cannot change rendered output or perturb replay.
    pub(super) fn observe_at(&mut self, now: Instant) {
        // Zero-watcher fast path: a single bool (the `subscribers.any()` analog).
        if !self.watchers.has_armed() {
            return;
        }
        let seq = self.content_seq();
        // The ACTIVE-GRID identity travels with the reading: `content_seq` is
        // per-grid, so a 1049 swap makes the raw counter incomparable.
        let alt = self.is_alternate_screen();
        let advanced = alt != self.watchers.seen_alt() || seq > self.watchers.seen_seq();
        // Walk the command blocks ONLY when a BlockComplete watcher is armed.
        let newest_complete = self.watchers.has_block_complete()
            && self.all_blocks().last().is_some_and(|b| {
                matches!(
                    b.state,
                    super::BlockState::PromptOnly | super::BlockState::Complete
                )
            });
        // Dirty-row gate: materialize visible-row text ONLY when a row scan will
        // run, and hand it to `observe` by reference so a matched row is never
        // re-cloned. Instead of a fresh `Vec<Option<String>>` + one freshly
        // heap-allocated `String` per visible row every batch (the eager
        // `.collect()` that defeated `observe`'s early-break and re-allocated for
        // the whole duration of a streaming `await match`), refill a PERSISTENT
        // scratch buffer: take it out of `self` (so the row-text reads borrow
        // `&self` while `observe` borrows `&mut self.watchers` — disjoint), clear
        // and refill each slot via the non-allocating `row_text_into`, then move
        // the buffer back. Both the outer `Vec` and every inner `String`'s heap
        // capacity survive across batches.
        if self.watchers.wants_row_scan(advanced) {
            let rows = self.rows() as usize;
            let mut scratch = std::mem::take(&mut self.row_text_scratch);
            scratch.resize_with(rows, || None);
            for (i, slot) in scratch.iter_mut().enumerate() {
                // Reuse the inner String's heap capacity (clear-then-append).
                // `row_text_into` clears+refills `s` and returns `true` iff the
                // row was in bounds (always so for `i < rows()`); on the
                // unreachable out-of-bounds case `s` is left empty and dropped,
                // mirroring `row_text`'s `None`.
                let mut s = slot.take().unwrap_or_default();
                *slot = self.row_text_into(i, &mut s).then_some(s);
            }
            self.watchers
                .observe_in(seq, alt, newest_complete, now, &scratch);
            self.row_text_scratch = scratch;
        } else {
            // Gate closed: no row text needed this batch.
            self.watchers
                .observe_in(seq, alt, newest_complete, now, &[]);
        }
    }

    /// Arm a surface watcher (the L1 `await`/`subscribe` verbs call this). `now`
    /// is the host's arming instant. Returns `None` (fail-closed) if the
    /// per-session watcher budget is full. A level-triggered spec
    /// ([`WatcherSpec::needs_arm_eval`] — `RowMatches` and `SeqAdvanced`) is
    /// evaluated immediately, so an already-matching row or an already-advanced
    /// `content_seq` latches at arm instead of waiting for the next batch.
    #[must_use]
    pub fn watch(&mut self, spec: WatcherSpec, now: Instant) -> Option<WatchId> {
        let arm_eval = spec.needs_arm_eval();
        // Seed the activity baseline to the CURRENT content_seq so the first
        // observe after arming does not read a PHANTOM advance (the kernel clock
        // defaults to 0, which would otherwise look like a fresh content jump and
        // spuriously reset an `IdleFor` deadline).
        let seq = self.content_seq();
        self.watchers.seed_seq_in(seq, self.is_alternate_screen());
        let id = self.watchers.arm(spec, now)?;
        if arm_eval {
            // Note the ordering: `seed_seq` above has already set the activity
            // baseline to `seq`, so this pass sees `advanced == false` and does
            // NOT stamp phantom activity (which would reset a concurrent
            // `IdleFor` deadline). `SeqAdvanced` latches on `content_seq >
            // after`, which is independent of `advanced`, so the arm-time
            // evaluation is correct precisely because it is activity-neutral.
            self.observe_at(now);
        }
        Some(id)
    }

    /// Convenience for the common row predicate: latch when any visible row in
    /// `rows` matches `matcher` (built in `aterm-observe`, regex out of core).
    #[must_use]
    pub fn watch_rows(
        &mut self,
        matcher: Arc<dyn RowMatch>,
        rows: RowRange,
        now: Instant,
    ) -> Option<WatchId> {
        self.watch(WatcherSpec::RowMatches { matcher, rows }, now)
    }

    /// Non-blocking: has watcher `id` latched? `None` if pending or unknown.
    #[must_use]
    pub fn watch_poll(&self, id: WatchId) -> Option<Satisfaction> {
        self.watchers.poll(id)
    }

    /// Remove a watcher. Idempotent.
    pub fn watch_disarm(&mut self, id: WatchId) {
        self.watchers.disarm(id);
    }

    /// The soonest pending `IdleFor` deadline — the L1 verb bounds its park here.
    #[must_use]
    pub fn watch_next_deadline(&self) -> Option<Instant> {
        self.watchers.next_deadline()
    }

    /// Host-driven idle firing: latch any `IdleFor` whose deadline `<= now`.
    pub fn watch_expire(&mut self, now: Instant) -> bool {
        self.watchers.expire(now)
    }

    /// `true` iff any watcher is armed (the producer's wake fan-out gate).
    #[must_use]
    pub fn watchers_armed(&self) -> bool {
        self.watchers.has_armed()
    }
}

/// The local engine IS a [`SurfaceSource`] (the 0-hop case of the L3 boundary):
/// predicate evaluation runs against the authoritative engine surface. A future
/// remote, astream-folded surface would implement the SAME trait. (Inherent
/// methods shadow the trait methods, so these do not recurse.)
impl SurfaceSource for super::Terminal {
    fn content_seq(&self) -> u64 {
        self.content_seq()
    }
    fn newest_block_complete(&self) -> bool {
        self.all_blocks().last().is_some_and(|b| {
            matches!(
                b.state,
                super::BlockState::PromptOnly | super::BlockState::Complete
            )
        })
    }
    fn rows(&self) -> usize {
        self.rows() as usize
    }
    fn row_text(&self, idx: usize) -> Option<String> {
        self.row_text(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now() // CLOCK-EXEMPT: test seed; all deltas below are explicit
    }

    /// No row text: the scalar predicates do not read rows.
    const NO_ROWS: &[Option<String>] = &[];

    /// THE ARM-TIME CONTRACT, at the level the bug actually lived.
    ///
    /// `await seq <n>` was edge-triggered for months: [`WatcherSet::arm`] set its
    /// `fresh` flag from `spec.is_row()`, so only `RowMatches` was evaluated at arm
    /// and `SeqAdvanced` waited for the NEXT unrelated batch — forever on a quiet
    /// session. An agent that recorded a seq last turn and asked "did anything
    /// change?" was told no while a screenful of output sat on the surface, and it
    /// failed as a plausible `OK timeout` rather than an error.
    ///
    /// The predicate arithmetic was never wrong, so a `WatcherSet`-only test cannot
    /// catch this: the defect was the WIRING. These two tests pin the wiring —
    /// which predicates are level-triggered, and that `Terminal::watch` really
    /// evaluates them before returning.
    #[test]
    fn needs_arm_eval_is_exactly_the_level_triggered_predicates() {
        // Statements about the surface as it ALREADY is -> must latch at arm.
        assert!(WatcherSpec::SeqAdvanced { after: 0 }.needs_arm_eval());
        // ...and this is the assertion the old `is_row()` wiring failed.
        assert!(
            WatcherSpec::SeqAdvanced { after: 7 }.needs_arm_eval(),
            "SeqAdvanced is level-triggered; arming must not wait for the next batch"
        );
        // Deliberately edge/deadline-triggered: idle is arm-relative, and
        // BlockComplete latching at arm would fire `await block` on the PREVIOUS
        // block.
        assert!(
            !WatcherSpec::IdleFor {
                dur: Duration::from_millis(1)
            }
            .needs_arm_eval()
        );
        assert!(!WatcherSpec::BlockComplete.needs_arm_eval());
    }

    #[test]
    fn watch_latches_an_already_advanced_seq_before_it_returns() {
        let mut term = crate::terminal::Terminal::new(6, 20);
        // Advance content_seq the way real output does, then STOP: no further batch
        // will arrive. This is the quiet-session shape that hid the bug.
        term.process(b"hello");
        let seq = term.content_seq();
        assert!(seq > 0, "feeding bytes must advance content_seq");

        // Ask the question an agent asks between turns: "anything since 0?"
        let id = term
            .watch(WatcherSpec::SeqAdvanced { after: 0 }, t0())
            .expect("watcher budget");
        let latched = term
            .watch_poll(id)
            .expect("an ALREADY-advanced seq must latch at arm, with no new batch");
        assert_eq!(latched.seq, seq);

        // The converse must still hold, or we have traded a false negative for a
        // false positive: nothing has happened since `seq`, so this stays pending.
        let id2 = term
            .watch(WatcherSpec::SeqAdvanced { after: seq }, t0())
            .expect("watcher budget");
        assert!(
            term.watch_poll(id2).is_none(),
            "arming at the current seq must NOT latch — nothing has changed yet"
        );
    }

    #[test]
    fn seq_advanced_latches_at_the_batch() {
        let base = t0();
        let mut w = WatcherSet::default();
        let id = w.arm(WatcherSpec::SeqAdvanced { after: 5 }, base).unwrap();
        assert!(w.poll(id).is_none(), "pending before any advance");
        w.observe(5, false, base + Duration::from_millis(1), NO_ROWS);
        assert!(w.poll(id).is_none());
        let at = base + Duration::from_millis(2);
        w.observe(7, false, at, NO_ROWS);
        assert_eq!(w.poll(id), Some(Satisfaction { seq: 7, at }));
    }

    #[test]
    fn idle_latches_at_the_deadline_not_the_observation_instant() {
        let base = t0();
        let d = Duration::from_millis(400);
        let mut w = WatcherSet::default();
        let id = w.arm(WatcherSpec::IdleFor { dur: d }, base).unwrap();
        let act = base + Duration::from_millis(100);
        w.observe(1, false, act, NO_ROWS);
        assert_eq!(w.next_deadline(), Some(act + d));
        let woke_late = act + d + Duration::from_millis(999);
        assert!(w.expire(woke_late));
        assert_eq!(
            w.poll(id),
            Some(Satisfaction {
                seq: 1,
                at: act + d
            })
        );
    }

    #[test]
    fn live_and_replay_latch_identically() {
        let base = t0();
        let d = Duration::from_millis(250);
        let schedule = [
            (1u64, Duration::from_millis(10)),
            (2, Duration::from_millis(20)),
            (3, Duration::from_millis(35)),
        ];
        let run = |expire_at: Duration| {
            let mut w = WatcherSet::default();
            let id = w.arm(WatcherSpec::IdleFor { dur: d }, base).unwrap();
            for (seq, off) in schedule {
                w.observe(seq, false, base + off, NO_ROWS);
            }
            w.expire(base + expire_at);
            w.poll(id)
        };
        let live = run(Duration::from_millis(35) + d + Duration::from_millis(2));
        let replay = run(Duration::from_millis(5000));
        assert_eq!(
            live, replay,
            "live and replay must latch the identical instant"
        );
        assert_eq!(live.unwrap().at, base + Duration::from_millis(35) + d);
    }

    #[test]
    fn idle_armed_on_a_stale_quiet_surface_waits_the_full_window() {
        let base = t0();
        let d = Duration::from_millis(500);
        let mut w = WatcherSet::default();
        w.observe(1, false, base, NO_ROWS);
        let arm_at = base + Duration::from_millis(5000);
        let id = w.arm(WatcherSpec::IdleFor { dur: d }, arm_at).unwrap();
        assert!(!w.expire(arm_at), "must not latch at arm on stale idleness");
        assert!(w.poll(id).is_none());
        assert!(w.expire(arm_at + d));
        assert_eq!(w.poll(id).unwrap().at, arm_at + d);
    }

    #[test]
    fn activity_resets_the_idle_deadline() {
        let base = t0();
        let d = Duration::from_millis(100);
        let mut w = WatcherSet::default();
        let id = w.arm(WatcherSpec::IdleFor { dur: d }, base).unwrap();
        w.observe(1, false, base + Duration::from_millis(50), NO_ROWS);
        assert!(!w.expire(base + Duration::from_millis(120)));
        assert!(w.poll(id).is_none());
        w.observe(2, false, base + Duration::from_millis(130), NO_ROWS);
        assert!(!w.expire(base + Duration::from_millis(200)));
        assert!(w.expire(base + Duration::from_millis(230)));
        assert_eq!(w.poll(id).unwrap().at, base + Duration::from_millis(230));
    }

    #[test]
    fn next_deadline_is_the_minimum() {
        let base = t0();
        let mut w = WatcherSet::default();
        let near = w
            .arm(
                WatcherSpec::IdleFor {
                    dur: Duration::from_millis(100),
                },
                base,
            )
            .unwrap();
        let _far = w
            .arm(
                WatcherSpec::IdleFor {
                    dur: Duration::from_millis(300),
                },
                base,
            )
            .unwrap();
        assert_eq!(w.next_deadline(), Some(base + Duration::from_millis(100)));
        w.expire(base + Duration::from_millis(150));
        assert!(w.poll(near).is_some());
        assert_eq!(w.next_deadline(), Some(base + Duration::from_millis(300)));
    }

    #[test]
    fn block_complete_latches_on_first_complete() {
        let base = t0();
        let mut w = WatcherSet::default();
        let id = w.arm(WatcherSpec::BlockComplete, base).unwrap();
        w.observe(1, false, base + Duration::from_millis(1), NO_ROWS);
        assert!(w.poll(id).is_none());
        w.observe(2, true, base + Duration::from_millis(2), NO_ROWS);
        assert_eq!(w.poll(id).unwrap().seq, 2);
    }

    #[test]
    fn row_matches_latches_in_the_one_list_on_content_advance() {
        // RowMatches is now a first-class WatcherSpec in the SAME list/path.
        #[derive(Debug)]
        struct Contains(&'static str);
        impl RowMatch for Contains {
            fn matches(&self, row: &str) -> bool {
                row.contains(self.0)
            }
        }
        let base = t0();
        let mut w = WatcherSet::default();
        let rows = [
            Some("booting".to_string()),
            Some("still booting".to_string()),
        ];
        let id = w
            .arm(
                WatcherSpec::RowMatches {
                    matcher: Arc::new(Contains("READY")),
                    rows: RowRange::All,
                },
                base,
            )
            .unwrap();
        // No matching row yet (and content advanced so the gate scans).
        w.observe(1, false, base, &rows);
        assert!(w.poll(id).is_none());
        // The row appears on the next advance -> latched.
        let rows2 = [Some("done".to_string()), Some("READY ❯".to_string())];
        w.observe(2, false, base, &rows2);
        assert_eq!(w.poll(id).unwrap().seq, 2);
    }

    #[test]
    fn arm_is_bounded_and_fails_closed() {
        let base = t0();
        let mut w = WatcherSet::with_capacity(2);
        assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_some());
        assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_some());
        assert!(
            w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_none(),
            "third arm past capacity fails closed"
        );
    }

    #[test]
    fn disarm_frees_a_slot() {
        let base = t0();
        let mut w = WatcherSet::with_capacity(1);
        let id = w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).unwrap();
        assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_none());
        w.disarm(id);
        assert!(w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).is_some());
        assert!(w.poll(id).is_none(), "disarmed id no longer known");
    }

    #[test]
    fn surface_source_boundary_is_transport_agnostic() {
        struct RemoteSurface {
            rows: Vec<String>,
        }
        impl SurfaceSource for RemoteSurface {
            fn content_seq(&self) -> u64 {
                7
            }
            fn newest_block_complete(&self) -> bool {
                false
            }
            fn rows(&self) -> usize {
                self.rows.len()
            }
            fn row_text(&self, idx: usize) -> Option<String> {
                self.rows.get(idx).cloned()
            }
        }
        #[derive(Debug)]
        struct Contains(&'static str);
        impl RowMatch for Contains {
            fn matches(&self, row: &str) -> bool {
                row.contains(self.0)
            }
        }
        let remote = RemoteSurface {
            rows: vec!["booting".into(), "❯ ready".into(), "idle".into()],
        };
        assert_eq!(
            first_matching_row(&remote, &Contains("❯"), RowRange::All),
            Some(1),
            "the kernel's row-match logic runs on a remote surface unchanged"
        );
        assert_eq!(
            first_matching_row(&remote, &Contains("nope"), RowRange::All),
            None
        );
    }

    /// Scratch-reuse correctness: a `RowMatches` watcher driven through the real
    /// `observe_at` seam over MANY processed batches stays pending until a
    /// visible row matches, then latches — proving the persistent
    /// `row_text_scratch` (clear-then-refill via `row_text_into`, reused across
    /// batches) observes exactly the same row text the old per-batch
    /// `Vec<Option<String>>` + per-row `String` allocation did. This is the
    /// streaming `aterm-drive await match` scenario: content_seq advances on
    /// every output batch, so the row scan runs (and refills the scratch) every
    /// batch for the whole stream.
    #[test]
    fn observe_at_reuses_scratch_across_batches_and_still_latches() {
        use crate::terminal::Terminal;

        #[derive(Debug)]
        struct Contains(&'static str);
        impl RowMatch for Contains {
            fn matches(&self, row: &str) -> bool {
                row.contains(self.0)
            }
        }

        let now = t0();
        let mut term = Terminal::new(4, 40);
        let id = term
            .watch_rows(Arc::new(Contains("BUILD SUCCESSFUL")), RowRange::All, now)
            .expect("arm the row watcher");

        // Several non-matching output batches: each drives observe_at, which now
        // refills the persistent scratch instead of allocating fresh. The
        // watcher must stay pending across all of them.
        term.process(b"Compiling...\r\n");
        assert!(term.watch_poll(id).is_none(), "pending while building (1)");
        term.process(b"Linking...\r\n");
        assert!(term.watch_poll(id).is_none(), "pending while building (2)");
        term.process(b"Running tests...\r\n");
        assert!(term.watch_poll(id).is_none(), "pending while building (3)");

        // The matching row finally appears -> the watcher latches at that batch.
        term.process(b"BUILD SUCCESSFUL");
        assert!(
            term.watch_poll(id).is_some(),
            "RowMatches must latch once a visible row matches, even with scratch reuse",
        );
    }
}
