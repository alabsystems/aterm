// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The observation kernel: event-driven "observe this surface until condition C holds".
//!
//! Every program byte folds into the grid and bumps one logical clock,
//! [`Terminal::content_seq`](super::Terminal::content_seq). Watchers are evaluated after each
//! batch's `post_process`, where every mutation has landed, and latched there, so a transiently
//! true condition is never lost to a coalesced wake. Every armed observer is a [`Watcher`] in
//! one list, distinguished by its [`WatcherSpec`]; [`WatcherSet::observe_screen`] evaluates all
//! kinds in one pass, and the deadline kinds also fire from [`WatcherSet::expire`]. All
//! predicates except `BlockComplete` (OSC 133/633 block state) work without shell integration,
//! alt-screen TUIs included.
//!
//! Properties:
//!
//! 1. No silent loss: a predicate that holds at any processed batch latches at that batch
//!    (`conformance_observe.rs`).
//! 2. Deterministic idle: `IdleFor` latches at the computed deadline (`activity_at + dur`), not
//!    the observation instant, so live and replay latch the same [`Satisfaction`]
//!    (`idle_deadline_model`).
//!
//! The kernel only observes: it never mutates the surface and is never checkpointed, so it
//! cannot affect replay. Its clock is the injected
//! [`process_now`](super::TransientState::process_now), never the wall clock.

use std::sync::Arc;
use std::time::Duration;

use aterm_time::Instant;

/// A handle to one armed watcher, unique within a [`WatcherSet`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WatchId(pub u64);

/// What one scan of one row established. A matcher that could not finish
/// reading a row (a bounded engine whose scan budget ran out) has learned
/// nothing about it, and says so rather than guessing either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowVerdict {
    /// The row matches.
    Match,
    /// The row was read to its end and does not match.
    NoMatch,
    /// The scan stopped before it could decide: evidence not gathered.
    Undecided,
}

impl From<bool> for RowVerdict {
    /// The verdict of a matcher that always finishes.
    fn from(matched: bool) -> Self {
        if matched {
            RowVerdict::Match
        } else {
            RowVerdict::NoMatch
        }
    }
}

/// An opaque, pre-compiled row matcher. The regex implementation lives in `aterm-observe`; the
/// core evaluates matches but cannot construct one, and takes no direct `regex` dependency.
///
/// A wait for a row to appear asks [`matches`](Self::matches); a wait for a row to leave asks
/// [`clears`](Self::clears). An [`Undecided`](RowVerdict::Undecided) row is neither, so both
/// kinds keep waiting until a later scan decides it. `clears` is the only spelling of absence:
/// `!matches(row)` would let `await gone`, `turn … settle=gone:` and the supervisor's busy-row
/// test latch while the row is still on screen.
pub trait RowMatch: Send + Sync + std::fmt::Debug {
    /// What scanning `row` (one visible row's text) established.
    fn verdict(&self, row: &str) -> RowVerdict;

    /// The scan showed that `row` matches. An undecided row does not.
    fn matches(&self, row: &str) -> bool {
        self.verdict(row) == RowVerdict::Match
    }

    /// The scan showed that `row` does not match. An undecided row does not.
    fn clears(&self, row: &str) -> bool {
        self.verdict(row) == RowVerdict::NoMatch
    }
}

/// An inclusive range of **visible** row indices to match against, or every
/// visible row. Constructed in `aterm-observe`; carried opaquely by the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowRange {
    /// Every visible row (`0..rows`).
    All,
    /// The inclusive visible-row span `start..=end`, intersected with the grid
    /// rather than clamped. A span covering no visible row (`start` past the
    /// grid, or `start > end`) is empty: `RowMatches` never latches on it and
    /// `RowsClear` refuses to (see [`covers_any`](Self::covers_any)); the verb
    /// layer reports `ERR bad rows`.
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

    /// Whether this range covers at least one of `visible_rows` grid rows. An
    /// empty range scans nothing, which is never evidence of a clear surface,
    /// so `RowsClear` will not latch on it.
    #[must_use]
    pub fn covers_any(self, visible_rows: usize) -> bool {
        match self {
            RowRange::All => visible_rows > 0,
            RowRange::Span { start, end } => start <= end && start < visible_rows,
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
    /// Latch after `dur` of no content mutation (quiescence, turn completion), at the exact
    /// deadline `activity_at + dur` however late it is observed.
    IdleFor {
        /// Latch after this much wall-time with no content mutation.
        dur: Duration,
    },
    /// Latch when the visible surface shows a completed/prompt-ready shell block.
    BlockComplete,
    /// Latch when any visible row in `rows` satisfies `matcher`.
    RowMatches {
        /// The opaque pre-compiled matcher.
        matcher: Arc<dyn RowMatch>,
        /// Which visible rows to scan.
        rows: RowRange,
    },
    /// Latch at a fixed instant the host computed at arm: the crossing of a decaying quantity
    /// below `floor` (the typing-momentum law `v·e^(−t/τ)`), for `await momentum` and
    /// `turn yield=`. Deadline-triggered like [`IdleFor`](WatcherSpec::IdleFor) but never reset
    /// by content activity; the arming verb re-reads the quantity when it fires and re-arms.
    MomentumBelow {
        /// The floor the quantity must have decayed to (documentary here; the
        /// host solved the crossing from it).
        floor: f32,
        /// The instant the quantity reaches `floor` under the release law.
        crossing: Instant,
    },
    /// Latch when no visible row in `rows` satisfies `matcher`: the inverse of
    /// [`RowMatches`](Self::RowMatches), for a "done" signal that is a row
    /// leaving (a busy footer). Level-triggered: no matching row at arm latches
    /// at arm.
    RowsClear {
        /// The opaque pre-compiled matcher.
        matcher: Arc<dyn RowMatch>,
        /// Which visible rows to scan.
        rows: RowRange,
    },
}

impl WatcherSpec {
    #[inline]
    fn is_row(&self) -> bool {
        matches!(
            self,
            WatcherSpec::RowMatches { .. } | WatcherSpec::RowsClear { .. }
        )
    }

    /// Whether the predicate is level-triggered and so evaluated once at arm:
    /// an already-matching row (`RowMatches`), an already-clear range
    /// (`RowsClear`) or an already-advanced `content_seq` (`SeqAdvanced`, an
    /// agent's "did anything change since my seq?") must answer yes without
    /// waiting for another batch.
    ///
    /// `IdleFor` is arm-relative (see [`WatcherSet::arm`]), and `BlockComplete`
    /// latching at arm would fire `await block` on the previous block.
    #[inline]
    fn needs_arm_eval(&self) -> bool {
        matches!(
            self,
            WatcherSpec::RowMatches { .. }
                | WatcherSpec::RowsClear { .. }
                | WatcherSpec::SeqAdvanced { .. }
        )
    }
}

/// A latched satisfaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Satisfaction {
    /// The `content_seq` in force when the predicate became true.
    pub seq: u64,
    /// The instant the predicate became true: the processed-batch instant for
    /// `SeqAdvanced`/`BlockComplete`/`RowMatches`/`RowsClear`, the computed
    /// deadline for `IdleFor`/`MomentumBelow`.
    pub at: Instant,
}

/// The read-only surface a watcher evaluates against, implemented by the local
/// engine ([`Terminal`](super::Terminal)). Remote aterms need not implement it:
/// `aterm_agent::RelayClient` relays `await` verbs to the remote's own
/// `WatcherSet`. Object-safe (owned returns) for an offline record/replay
/// observer.
pub trait SurfaceSource {
    /// The monotonic content clock (`content_seq`).
    fn content_seq(&self) -> u64;
    /// Whether the newest shell-integration block is complete/prompt-ready.
    fn newest_block_complete(&self) -> bool;
    /// The number of visible rows.
    fn rows(&self) -> usize;
    /// The text of visible row `idx` (owned, for object safety), or `None`: the
    /// live screen's row, as the control protocol's `text` serves it, never the
    /// row a person scrolled into view.
    fn row_text(&self, idx: usize) -> Option<String>;
}

/// The first visible row of `source` in `range` that `matcher` accepts: the
/// surface-agnostic core of `RowMatches`.
#[must_use]
pub fn first_matching_row(
    source: &dyn SurfaceSource,
    matcher: &dyn RowMatch,
    range: RowRange,
) -> Option<usize> {
    (0..source.rows())
        .find(|&i| range.contains(i) && source.row_text(i).is_some_and(|t| matcher.matches(&t)))
}

/// Whether any pre-collected visible row in `range` is shown to match
/// `matcher`: the `RowMatches` scan. Stops at the first hit.
#[must_use]
fn any_row_matches(rows: &[Option<String>], matcher: &dyn RowMatch, range: RowRange) -> bool {
    rows.iter().enumerate().any(|(idx, cell)| {
        range.contains(idx) && cell.as_deref().is_some_and(|t| matcher.matches(t))
    })
}

/// Whether every pre-collected visible row in `range` is shown not to match `matcher`: the
/// `RowsClear` scan. A row the matcher could not decide keeps the range from reading clear (see
/// [`RowMatch`]); a row with no text has nothing to match. Stops at the first row not clear.
#[must_use]
fn every_row_clear(rows: &[Option<String>], matcher: &dyn RowMatch, range: RowRange) -> bool {
    rows.iter().enumerate().all(|(idx, cell)| {
        !range.contains(idx) || cell.as_deref().is_none_or(|t| matcher.clears(t))
    })
}

/// The activity clock: the last `content_seq` seen to advance and the injected
/// instant at which it advanced. Never reads the wall clock.
#[derive(Clone, Copy, Debug, Default)]
struct ActivityClock {
    last_seq: u64,
    last_at: Option<Instant>,
    /// Which grid `last_seq` was read from. `content_seq` is per grid: entering the alt screen
    /// installs a grid whose counter restarts at 1, and leaving restores the main grid's higher
    /// value, so a flip is a resync rather than a comparison.
    alt: bool,
    /// The screen generation `last_seq` was read in: the terminal's
    /// [`invalidation_epoch`](super::ContentScrollState::invalidation_epoch),
    /// advanced by every screen switch, RIS and coordinate-moving batch. `alt`
    /// alone cannot see a leave-and-re-enter, whose fresh grid can reach the
    /// same seq; within one epoch the seq only grows, so `(epoch, seq)` never
    /// repeats (the pair `status gen=` publishes).
    epoch: u64,
}

/// One armed watcher — the single watcher type, distinguished only by `spec`.
#[derive(Clone)]
struct Watcher {
    id: WatchId,
    spec: WatcherSpec,
    /// For `IdleFor`, the current fire deadline (`activity_at + dur`), recomputed on each
    /// content advance; for `MomentumBelow`, the fixed crossing. `None` for the other specs.
    deadline: Option<Instant>,
    /// `Some` once the predicate has held; sticky while armed.
    latched: Option<Satisfaction>,
    /// A freshly armed row watcher is scanned once even without a content advance, so an
    /// already-matching row (or already-clear range) latches at arm. Cleared after the first
    /// `observe_screen`.
    fresh: bool,
    /// Which grid this watcher was armed against: `SeqAdvanced`'s `after` is per grid, so
    /// crossing a buffer swap latches; otherwise `await seq` would starve on the alt screen and
    /// fire spuriously on 1049 exit.
    armed_alt: bool,
    /// Which screen generation this watcher was armed in
    /// ([`ActivityClock::epoch`]); crossing an epoch step latches too.
    armed_epoch: u64,
}

/// A bounded set of armed watchers plus the activity clock, held in
/// [`Terminal`](super::Terminal): never checkpointed, never mutates the surface.
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

    /// The screen generation the kernel last saw (`ActivityClock::epoch`).
    #[must_use]
    #[cfg(test)]
    pub fn seen_epoch(&self) -> u64 {
        self.clock.epoch
    }

    /// Whether `(seq, alt, epoch)` is a surface change from what the kernel last
    /// saw: another grid, another generation, or a higher seq on the same one.
    /// The one change test, shared by the row-scan gate and
    /// [`observe_screen`](Self::observe_screen).
    #[must_use]
    pub fn is_advance(&self, seq: u64, alt: bool, epoch: u64) -> bool {
        alt != self.clock.alt || epoch != self.clock.epoch || seq > self.clock.last_seq
    }

    /// Whether a row scan would do work this batch: some un-latched row watcher
    /// is fresh or content advanced. The engine collects row text only then.
    #[must_use]
    pub fn wants_row_scan(&self, advanced: bool) -> bool {
        self.watchers
            .iter()
            .any(|w| w.latched.is_none() && w.spec.is_row() && (advanced || w.fresh))
    }

    /// Arm a watcher, returning its handle, or `None` (fail-closed) at capacity.
    /// `now` seeds the activity baseline so an `IdleFor` armed on an already
    /// quiet surface still has a deadline.
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
        // Idle is measured from arm: arming on an already-quiet surface waits the
        // full `dur` (reset by later activity) rather than firing on stale idleness.
        let deadline = match &spec {
            WatcherSpec::IdleFor { dur } => Some(now + *dur),
            // A fixed deadline: the crossing was solved by the host from the
            // reading at arm, and no later activity moves it.
            WatcherSpec::MomentumBelow { crossing, .. } => Some(*crossing),
            _ => None,
        };
        let fresh = spec.is_row();
        self.watchers.push(Watcher {
            id,
            spec,
            deadline,
            latched: None,
            fresh,
            // The clock's grid identity is the arming grid: `Terminal::watch`
            // seeds it (`seed_screen`) immediately before arming.
            armed_alt: self.clock.alt,
            armed_epoch: self.clock.epoch,
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

    /// Seed the activity baseline so the first `observe_screen` after arming does not
    /// read a phantom advance. Monotone: never lowers an existing baseline.
    pub fn seed_seq(&mut self, seq: u64) {
        if self.clock.last_seq < seq {
            self.clock.last_seq = seq;
        }
    }

    /// [`seed_seq`](Self::seed_seq) with the active-grid identity and screen
    /// generation: a new grid or generation sets the baseline rather than
    /// raising it, since monotonicity only holds within one generation of one
    /// buffer.
    pub fn seed_screen(&mut self, seq: u64, alt: bool, epoch: u64) {
        if alt != self.clock.alt || epoch != self.clock.epoch {
            self.clock.alt = alt;
            self.clock.epoch = epoch;
            self.clock.last_seq = seq;
            return;
        }
        self.seed_seq(seq);
    }

    /// Whether an un-latched `BlockComplete` watcher is armed; gates the
    /// O(blocks) shell-integration walk in `observe_at`.
    #[must_use]
    pub fn has_block_complete(&self) -> bool {
        self.watchers
            .iter()
            .any(|w| w.latched.is_none() && matches!(w.spec, WatcherSpec::BlockComplete))
    }

    /// The soonest pending `IdleFor` / `MomentumBelow` deadline, which the
    /// arming `await`/`ready` verb uses to bound its `Subscription::wait` park.
    /// `None` when no un-latched deadline watcher is armed.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.watchers
            .iter()
            .filter(|w| w.latched.is_none())
            .filter_map(|w| w.deadline)
            .min()
    }

    /// [`observe_screen`](Self::observe_screen) on the main grid at the current generation.
    #[cfg(test)]
    pub fn observe(
        &mut self,
        content_seq: u64,
        newest_block_complete: bool,
        now: Instant,
        rows: &[Option<String>],
    ) -> bool {
        let epoch = self.clock.epoch;
        self.observe_screen(content_seq, false, epoch, newest_block_complete, now, rows)
    }

    /// The seam call, run by `observe_at` after every batch (and at arm for a level-triggered
    /// spec) with `now == transient.process_now`. Stamps activity if the surface advanced and
    /// latches every predicate that holds, in one pass. A change of grid or generation is a
    /// resync: the baseline is set, not raised, since the counter is not comparable across it
    /// (`ActivityClock::epoch`). `rows[idx]` is visible-row text for the row predicates; the
    /// caller collects it only when [`wants_row_scan`](Self::wants_row_scan) holds, and an empty
    /// `rows` is never read as a clear surface. Returns `true` if anything latched.
    pub fn observe_screen(
        &mut self,
        content_seq: u64,
        alt: bool,
        epoch: u64,
        newest_block_complete: bool,
        now: Instant,
        rows: &[Option<String>],
    ) -> bool {
        let advanced = self.is_advance(content_seq, alt, epoch);
        if advanced {
            self.clock.last_seq = content_seq;
            self.clock.alt = alt;
            self.clock.epoch = epoch;
            self.clock.last_at = Some(now);
        }
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            // Decide the latch / deadline update with the spec borrow, then apply
            // the field writes after the match (disjoint-borrow safe).
            let mut new_latch: Option<Satisfaction> = None;
            let mut new_deadline: Option<Instant> = None;
            match &w.spec {
                WatcherSpec::SeqAdvanced { after } => {
                    // `after` is per grid: compare only within the same grid and
                    // generation; crossing a swap latches on the identity change.
                    if alt != w.armed_alt || epoch != w.armed_epoch || content_seq > *after {
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
                // Content activity is not this predicate's business: the
                // deadline was fixed at arm and fires from `expire` alone.
                WatcherSpec::MomentumBelow { .. } => {}
                WatcherSpec::RowMatches {
                    matcher,
                    rows: range,
                } => {
                    // Dirty-row gate: scan only on advance or a fresh arm.
                    if (advanced || w.fresh) && any_row_matches(rows, &**matcher, *range) {
                        new_latch = Some(Satisfaction {
                            seq: content_seq,
                            at: now,
                        });
                    }
                }
                WatcherSpec::RowsClear {
                    matcher,
                    rows: range,
                } => {
                    // Latch only when every row in range is shown not to match, on a batch with
                    // rows collected and a range meeting at least one: scanning nothing, or a
                    // row the matcher could not decide, is not evidence of a clear surface.
                    if (advanced || w.fresh)
                        && range.covers_any(rows.len())
                        && every_row_clear(rows, &**matcher, *range)
                    {
                        new_latch = Some(Satisfaction {
                            seq: content_seq,
                            at: now,
                        });
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

    /// Restart every un-latched `IdleFor` deadline at `now + dur`: unseen time is
    /// not quiet time. The host calls this instead of [`Self::expire`] while this
    /// terminal's output is not being consumed (its reader parked for an
    /// update), since the program may still be printing.
    pub fn restart_idle(&mut self, now: Instant) {
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            if let WatcherSpec::IdleFor { dur } = w.spec {
                w.deadline = Some(now + dur);
            }
        }
    }

    /// The idle-fire call, run when the host's `WaitUntil` wake reaches `now`
    /// (and once at a replay target). Latches every un-latched `IdleFor` /
    /// `MomentumBelow` whose deadline has passed with `at = deadline`, so the
    /// result does not depend on when the host woke. Returns `true` if anything
    /// latched.
    pub fn expire(&mut self, now: Instant) -> bool {
        let last_seq = self.clock.last_seq;
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            if matches!(
                w.spec,
                WatcherSpec::IdleFor { .. } | WatcherSpec::MomentumBelow { .. }
            ) {
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
    /// Run the observation kernel for the batch just processed: called from
    /// [`process_at`](super::Terminal::process_at) after `post_process` (and
    /// from [`watch`](Self::watch) at arm) with the injected clock. Read-only on
    /// the surface.
    pub(super) fn observe_at(&mut self, now: Instant) {
        if !self.watchers.has_armed() {
            return;
        }
        let seq = self.content_seq();
        // `content_seq` is per grid: a 1049 swap makes the raw counter incomparable.
        let alt = self.is_alternate_screen();
        // And the screen generation: a leave-and-re-enter keeps `alt` and can
        // repeat `seq`; only the epoch says the grid is another one.
        let epoch = self.content_scroll_state.invalidation_epoch;
        let advanced = self.watchers.is_advance(seq, alt, epoch);
        // Walk the command blocks only when a BlockComplete watcher is armed.
        let newest_complete = self.watchers.has_block_complete()
            && self.all_blocks().last().is_some_and(|b| {
                matches!(
                    b.state,
                    super::BlockState::PromptOnly | super::BlockState::Complete
                )
            });
        // Collect visible-row text only when a row scan will run, into a persistent scratch
        // taken out of `self` (row reads borrow `&self` while `observe_screen` borrows
        // `self.watchers`); the `Vec` and each `String`'s capacity are reused across batches.
        // The live screen's rows, as `text` serves them: a person scrolled back must not make
        // a live box look gone, nor an old copy of it in history look live.
        if self.watchers.wants_row_scan(advanced) {
            let rows = self.rows() as usize;
            let mut scratch = std::mem::take(&mut self.row_text_scratch);
            scratch.resize_with(rows, || None);
            for (i, slot) in scratch.iter_mut().enumerate() {
                let mut s = slot.take().unwrap_or_default();
                *slot = self.live_row_text_into(i, &mut s).then_some(s);
            }
            self.watchers
                .observe_screen(seq, alt, epoch, newest_complete, now, &scratch);
            self.row_text_scratch = scratch;
        } else {
            // Gate closed: no row text needed this batch.
            self.watchers
                .observe_screen(seq, alt, epoch, newest_complete, now, &[]);
        }
    }

    /// Arm a surface watcher (called by the `await`/`subscribe` verbs). `now` is
    /// the arming instant. `None` (fail-closed) when the per-session budget is
    /// full. A level-triggered spec ([`WatcherSpec::needs_arm_eval`]) is
    /// evaluated immediately.
    #[must_use]
    pub fn watch(&mut self, spec: WatcherSpec, now: Instant) -> Option<WatchId> {
        let arm_eval = spec.needs_arm_eval();
        // Seed the baseline to the current content_seq so the first observe does
        // not read a phantom advance and spuriously reset an `IdleFor` deadline.
        let seq = self.content_seq();
        let epoch = self.content_scroll_state.invalidation_epoch;
        self.watchers
            .seed_screen(seq, self.is_alternate_screen(), epoch);
        let id = self.watchers.arm(spec, now)?;
        if arm_eval {
            // The baseline was just seeded, so this pass sees `advanced == false` and stamps no
            // activity; `SeqAdvanced` latches on `content_seq > after` regardless.
            self.observe_at(now);
        }
        Some(id)
    }

    /// Convenience for the common row predicate: latch when any visible row in
    /// `rows` matches `matcher` (built in `aterm-observe`).
    #[must_use]
    pub fn watch_rows(
        &mut self,
        matcher: Arc<dyn RowMatch>,
        rows: RowRange,
        now: Instant,
    ) -> Option<WatchId> {
        self.watch(WatcherSpec::RowMatches { matcher, rows }, now)
    }

    /// The inverse of [`watch_rows`](Self::watch_rows): latch when no visible row
    /// in `rows` matches `matcher` (`await gone`). Level-triggered.
    #[must_use]
    pub fn watch_rows_gone(
        &mut self,
        matcher: Arc<dyn RowMatch>,
        rows: RowRange,
        now: Instant,
    ) -> Option<WatchId> {
        self.watch(WatcherSpec::RowsClear { matcher, rows }, now)
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

    /// The soonest pending deadline ([`WatcherSet::next_deadline`]).
    #[must_use]
    pub fn watch_next_deadline(&self) -> Option<Instant> {
        self.watchers.next_deadline()
    }

    /// Host-driven deadline firing ([`WatcherSet::expire`]).
    pub fn watch_expire(&mut self, now: Instant) -> bool {
        self.watchers.expire(now)
    }

    /// Idle hold: while the host consumes none of this terminal's output,
    /// restart every pending `IdleFor` at `now` ([`WatcherSet::restart_idle`]).
    pub fn watch_restart_idle(&mut self, now: Instant) {
        self.watchers.restart_idle(now);
    }

    /// `true` iff any watcher is armed (the producer's wake fan-out gate).
    #[must_use]
    pub fn watchers_armed(&self) -> bool {
        self.watchers.has_armed()
    }
}

/// The local engine is a [`SurfaceSource`]. (Inherent methods shadow the trait
/// methods, so these do not recurse.)
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
        let mut s = String::new();
        self.live_row_text_into(idx, &mut s).then_some(s)
    }
}

impl super::Terminal {
    /// Live screen row `idx`'s text into `out` (cleared first), whatever the display
    /// offset: the frame the control protocol's `text` and the input fence's `if-fp=`
    /// read, where [`Terminal::row_text_into`](super::Terminal::row_text_into) follows
    /// the person's scroll. `false` for a row past the screen.
    fn live_row_text_into(&self, idx: usize, out: &mut String) -> bool {
        if idx >= usize::from(self.grid.rows()) {
            out.clear();
            return false;
        }
        self.grid
            .row_text_screen_into(crate::grid::row_u16(idx), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now() // test seed; all deltas below are explicit
    }

    /// No row text: the scalar predicates do not read rows.
    const NO_ROWS: &[Option<String>] = &[];

    /// Exactly the level-triggered predicates are evaluated at arm (`Terminal::watch`).
    #[test]
    fn needs_arm_eval_is_exactly_the_level_triggered_predicates() {
        // Statements about the surface as it already is -> must latch at arm.
        assert!(WatcherSpec::SeqAdvanced { after: 0 }.needs_arm_eval());
        #[derive(Debug)]
        struct Never;
        impl RowMatch for Never {
            fn verdict(&self, _: &str) -> RowVerdict {
                RowVerdict::NoMatch
            }
        }
        assert!(
            WatcherSpec::RowMatches {
                matcher: Arc::new(Never),
                rows: RowRange::All,
            }
            .needs_arm_eval()
        );
        assert!(
            WatcherSpec::RowsClear {
                matcher: Arc::new(Never),
                rows: RowRange::All,
            }
            .needs_arm_eval(),
            "RowsClear is level-triggered: an already-clear surface latches at arm"
        );
        assert!(
            WatcherSpec::SeqAdvanced { after: 7 }.needs_arm_eval(),
            "SeqAdvanced is level-triggered; arming must not wait for the next batch"
        );
        // Edge/deadline-triggered.
        assert!(
            !WatcherSpec::IdleFor {
                dur: Duration::from_millis(1)
            }
            .needs_arm_eval()
        );
        assert!(!WatcherSpec::BlockComplete.needs_arm_eval());
    }

    /// `MomentumBelow` latches at its armed crossing whenever the host wakes, and content
    /// activity does not move it; an `IdleFor` beside it is reset by the same activity.
    #[test]
    fn a_momentum_watcher_latches_at_its_crossing_and_content_activity_does_not_move_it() {
        let base = t0();
        let mut set = WatcherSet::default();
        let crossing = base + Duration::from_millis(500);
        let id = set
            .arm(
                WatcherSpec::MomentumBelow {
                    floor: 0.25,
                    crossing,
                },
                base,
            )
            .expect("budget");
        let idle = set
            .arm(
                WatcherSpec::IdleFor {
                    dur: Duration::from_millis(500),
                },
                base,
            )
            .expect("budget");
        assert_eq!(set.next_deadline(), Some(crossing));

        // The human types: content advances at +300 ms. The idle watcher's
        // deadline moves to +800 ms; the crossing does not move at all.
        let typed = base + Duration::from_millis(300);
        assert!(!set.observe(7, false, typed, NO_ROWS));
        assert_eq!(
            set.next_deadline(),
            Some(crossing),
            "a keystroke must not move the analytic crossing"
        );
        assert!(!set.expire(typed), "nothing has crossed yet");
        assert!(set.poll(id).is_none());

        // A late wake (+900 ms): the latch records the crossing, not the wake.
        let late = base + Duration::from_millis(900);
        assert!(set.expire(late));
        let sat = set.poll(id).expect("crossed");
        assert_eq!(sat.at, crossing, "latched at the deadline, not at `now`");
        assert_eq!(sat.seq, 7, "the seq the clock last saw");
        // The idle watcher latched too, at its moved deadline (+800 ms).
        assert_eq!(
            set.poll(idle).map(|s| s.at),
            Some(typed + Duration::from_millis(500))
        );
    }

    #[test]
    fn watch_latches_an_already_advanced_seq_before_it_returns() {
        let mut term = crate::terminal::Terminal::new(6, 20);
        // Output, then a quiet session: no further batch arrives.
        term.process(b"hello");
        let seq = term.content_seq();
        assert!(seq > 0, "feeding bytes must advance content_seq");

        let id = term
            .watch(WatcherSpec::SeqAdvanced { after: 0 }, t0())
            .expect("watcher budget");
        let latched = term
            .watch_poll(id)
            .expect("an ALREADY-advanced seq must latch at arm, with no new batch");
        assert_eq!(latched.seq, seq);

        // The converse: nothing since `seq`, so this stays pending.
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

    /// A restarted idle watcher waits its whole window again, and a latched one
    /// keeps its latch. Negative control: without the restart it fires at once.
    #[test]
    fn a_restarted_idle_waits_its_whole_window_again() {
        let base = t0();
        let d = Duration::from_millis(250);
        let mut w = WatcherSet::default();
        let id = w.arm(WatcherSpec::IdleFor { dur: d }, base).unwrap();
        let later = base + Duration::from_millis(1000);
        let mut control = w.clone();
        control.expire(later);
        assert!(control.poll(id).is_some(), "the window passed unseen");
        w.restart_idle(later);
        w.expire(later + d - Duration::from_millis(1));
        assert!(
            w.poll(id).is_none(),
            "not before a whole window from the restart"
        );
        w.expire(later + d);
        assert_eq!(w.poll(id).map(|s| s.at), Some(later + d));
        w.restart_idle(later + Duration::from_secs(10));
        assert_eq!(w.poll(id).map(|s| s.at), Some(later + d), "a latch stays");
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

    /// Equal-seq re-entry: watchers armed at `(seq 15, alt, epoch 1)` see
    /// `(seq 15, alt, epoch 2)` as a change. Negative control: the same reading
    /// at the same epoch scans and latches nothing.
    #[test]
    fn a_new_screen_generation_at_an_equal_seq_is_an_advance() {
        #[derive(Debug)]
        struct Contains(&'static str);
        impl RowMatch for Contains {
            fn verdict(&self, row: &str) -> RowVerdict {
                row.contains(self.0).into()
            }
        }
        let base = t0();
        let box_b = [Some("rm -rf /tmp/work".to_string())];
        for (epoch, moves) in [(1, false), (2, true)] {
            let mut w = WatcherSet::default();
            w.seed_screen(15, true, 1);
            let rows = w
                .arm(
                    WatcherSpec::RowMatches {
                        matcher: Arc::new(Contains("rm -rf")),
                        rows: RowRange::All,
                    },
                    base,
                )
                .unwrap();
            let seq = w.arm(WatcherSpec::SeqAdvanced { after: 15 }, base).unwrap();
            // The arm-time scan (box A on screen).
            w.observe_screen(15, true, 1, false, base, &[Some("ls".to_string())]);
            assert!(w.poll(rows).is_none() && w.poll(seq).is_none());
            assert_eq!(w.is_advance(15, true, epoch), moves, "epoch {epoch}");
            w.observe_screen(15, true, epoch, false, base, &box_b);
            assert_eq!(w.poll(rows).is_some(), moves, "row watcher, epoch {epoch}");
            assert_eq!(w.poll(seq).is_some(), moves, "seq watcher, epoch {epoch}");
            assert_eq!(w.seen_epoch(), epoch);
        }
    }

    /// The shared row matcher for the `RowsClear` tests below.
    #[derive(Debug)]
    struct Busy;
    impl RowMatch for Busy {
        fn verdict(&self, row: &str) -> RowVerdict {
            row.contains("esc to interrupt").into()
        }
    }

    fn rows_clear(range: RowRange) -> WatcherSpec {
        WatcherSpec::RowsClear {
            matcher: Arc::new(Busy),
            rows: range,
        }
    }

    #[test]
    fn rows_clear_latches_at_arm_when_nothing_matches() {
        let base = t0();
        let mut w = WatcherSet::default();
        // The kernel clock is already at 7: the arm-time scan sees no advance.
        w.seed_seq(7);
        let id = w.arm(rows_clear(RowRange::All), base).unwrap();
        let rows = [Some("done".to_string()), Some("❯ ".to_string())];
        w.observe(7, false, base, &rows);
        assert_eq!(
            w.poll(id).unwrap().seq,
            7,
            "no matching row at arm => latched on the fresh scan, same seq"
        );
    }

    /// A quiet frame in between, with no rows collected, is not read as clear.
    #[test]
    fn rows_clear_holds_while_a_row_matches_then_latches_when_it_leaves() {
        let base = t0();
        let mut w = WatcherSet::default();
        let id = w.arm(rows_clear(RowRange::All), base).unwrap();
        let busy = [
            Some("thinking…".to_string()),
            Some("esc to interrupt".to_string()),
        ];
        w.observe(1, false, base, &busy);
        assert!(w.poll(id).is_none(), "a matching row keeps `gone` pending");
        // The footer is still there on the next repaint.
        let still_busy = [
            Some("still thinking…".to_string()),
            Some("(3s · esc to interrupt)".to_string()),
        ];
        w.observe(2, false, base, &still_busy);
        assert!(w.poll(id).is_none(), "still matching => still pending");
        // Quiescent frame: no advance, so the caller collects no rows.
        w.observe(2, false, base, NO_ROWS);
        assert!(
            w.poll(id).is_none(),
            "an un-scanned frame is not a clear surface"
        );
        let clear = [Some("Done.".to_string()), Some("❯ ".to_string())];
        w.observe(3, false, base, &clear);
        assert_eq!(w.poll(id).unwrap().seq, 3);
    }

    #[test]
    fn rows_clear_range_ignores_a_match_outside_the_range() {
        let base = t0();
        let mut w = WatcherSet::default();
        let rows = [
            Some("esc to interrupt".to_string()),
            Some("body".to_string()),
            Some("footer".to_string()),
        ];
        let outside = w
            .arm(rows_clear(RowRange::Span { start: 1, end: 2 }), base)
            .unwrap();
        let covering = w
            .arm(rows_clear(RowRange::Span { start: 0, end: 1 }), base)
            .unwrap();
        w.observe(1, false, base, &rows);
        assert_eq!(
            w.poll(outside).unwrap().seq,
            1,
            "the match on row 0 is outside rows 1..=2 => clear => latched"
        );
        assert!(
            w.poll(covering).is_none(),
            "the same match inside rows 0..=1 holds that watcher pending"
        );
    }

    /// An empty range keeps `RowsClear` pending: no reading is not a clear reading.
    #[test]
    fn rows_clear_never_latches_on_a_range_that_covers_no_visible_row() {
        let base = t0();
        let mut w = WatcherSet::default();
        let past_the_grid = w
            .arm(rows_clear(RowRange::Span { start: 40, end: 50 }), base)
            .unwrap();
        let inverted = w
            .arm(rows_clear(RowRange::Span { start: 2, end: 1 }), base)
            .unwrap();
        // A 3-row grid with nothing matching anywhere.
        let clear = [
            Some("Done.".to_string()),
            Some("❯ ".to_string()),
            Some(String::new()),
        ];
        w.observe(1, false, base, &clear);
        w.observe(2, false, base, &clear);
        assert!(
            w.poll(past_the_grid).is_none(),
            "rows 40..=50 on a 3-row grid meets no row: no vacuous latch"
        );
        assert!(
            w.poll(inverted).is_none(),
            "rows 2..=1 is inverted, so empty: no vacuous latch"
        );
        // The same clear surface under a span that does meet a row (2..=50 covers
        // row 2 and is intersected with the grid beyond it) latches at once.
        let covering = w
            .arm(rows_clear(RowRange::Span { start: 2, end: 50 }), base)
            .unwrap();
        w.observe(2, false, base, &clear);
        assert_eq!(
            w.poll(covering).unwrap().seq,
            2,
            "a span reaching past the grid is intersected with it, not emptied"
        );
        // The predicate the verb layer gates on, on a 24-row grid.
        assert!(!RowRange::Span { start: 40, end: 50 }.covers_any(24));
        assert!(!RowRange::Span { start: 2, end: 1 }.covers_any(24));
        assert!(RowRange::Span { start: 23, end: 99 }.covers_any(24));
        assert!(RowRange::All.covers_any(1));
        assert!(!RowRange::All.covers_any(0));
    }

    /// An undecided row (a bounded engine out of budget) neither fires `RowMatches` nor lets
    /// `RowsClear` latch; both answer once a later scan decides it.
    #[test]
    fn an_undecided_row_holds_both_row_predicates_pending() {
        /// Decides a row only when it is short: a stand-in for a scan budget.
        #[derive(Debug)]
        struct Budgeted;
        impl RowMatch for Budgeted {
            fn verdict(&self, row: &str) -> RowVerdict {
                if row.len() > 16 {
                    RowVerdict::Undecided
                } else {
                    row.contains("busy").into()
                }
            }
        }
        let base = t0();
        let mut w = WatcherSet::default();
        let busy = || Arc::new(Budgeted) as Arc<dyn RowMatch>;
        let shown = w
            .arm(
                WatcherSpec::RowMatches {
                    matcher: busy(),
                    rows: RowRange::All,
                },
                base,
            )
            .unwrap();
        let gone = w
            .arm(
                WatcherSpec::RowsClear {
                    matcher: busy(),
                    rows: RowRange::All,
                },
                base,
            )
            .unwrap();
        let heavy = [Some("done".to_string()), Some(format!("{:>40}", "busy"))];
        w.observe(1, false, base, &heavy);
        assert!(w.poll(shown).is_none(), "undecided is not a match");
        assert!(w.poll(gone).is_none(), "undecided is not clear");
        // The same row, now short enough to read: it matches.
        let light = [Some("done".to_string()), Some("busy".to_string())];
        w.observe(2, false, base, &light);
        assert_eq!(w.poll(shown).unwrap().seq, 2);
        assert!(w.poll(gone).is_none(), "a decided match holds `gone`");
        // It leaves: decided clear on every row, so `gone` latches.
        let clear = [Some("done".to_string()), Some("❯ ".to_string())];
        w.observe(3, false, base, &clear);
        assert_eq!(w.poll(gone).unwrap().seq, 3);
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

    /// The reused `row_text_scratch` sees the same text a fresh collection would.
    #[test]
    fn observe_at_reuses_scratch_across_batches_and_still_latches() {
        use crate::terminal::Terminal;

        #[derive(Debug)]
        struct Contains(&'static str);
        impl RowMatch for Contains {
            fn verdict(&self, row: &str) -> RowVerdict {
                row.contains(self.0).into()
            }
        }

        let now = t0();
        let mut term = Terminal::new(4, 40);
        let id = term
            .watch_rows(Arc::new(Contains("BUILD SUCCESSFUL")), RowRange::All, now)
            .expect("arm the row watcher");

        term.process(b"Compiling...\r\n");
        assert!(term.watch_poll(id).is_none(), "pending while building (1)");
        term.process(b"Linking...\r\n");
        assert!(term.watch_poll(id).is_none(), "pending while building (2)");
        term.process(b"Running tests...\r\n");
        assert!(term.watch_poll(id).is_none(), "pending while building (3)");

        term.process(b"BUILD SUCCESSFUL");
        assert!(
            term.watch_poll(id).is_some(),
            "RowMatches must latch once a visible row matches, even with scratch reuse",
        );
    }
}
