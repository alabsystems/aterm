// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The observation kernel: event-driven "observe this surface until condition C holds".

use std::sync::Arc;
use std::time::Duration;

use aterm_time::Instant;

/// Hang detector for an unfinished DEC 2026 frame ([`super::Terminal::frame_unfinished_at`]).
pub const FRAME_HANG: Duration = Duration::from_secs(60);

/// How long a range must read clear before `RowsClear` (`await gone`) latches on an unbracketed
/// screen.
pub const GONE_SETTLE: Duration = Duration::from_secs(1);

/// Whether the program's frame is whole at one observation pass, and when its hang detector fires
/// if it is not ([`WatcherSet::observe_screen`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// No row predicate may read the rows when `false`.
    pub whole: bool,
    /// Unfinished frame's hang detector ([`FRAME_HANG`] past the first batch of the run); `None`
    /// on a whole frame.
    pub hang: Option<Instant>,
    /// The screen is a closed frame with nothing drawn since
    /// ([`super::Terminal::screen_is_closed_frame`]), so `RowsClear` latches at once.
    pub closed: bool,
    /// When the screen last changed ([`super::Terminal::drawn_at`]), from which [`GONE_SETTLE`]
    /// runs at arm.
    pub drawn_at: Option<Instant>,
}

impl Frame {
    /// A frame the program closed: whole, and believed at once.
    pub const CLOSED: Self = Self {
        whole: true,
        hang: None,
        closed: true,
        drawn_at: None,
    };

    /// A whole frame no bracket vouches for, drawn at `drawn_at`, on which `RowsClear` settles.
    #[must_use]
    pub const fn unbracketed(drawn_at: Option<Instant>) -> Self {
        Self {
            whole: true,
            hang: None,
            closed: false,
            drawn_at,
        }
    }
}

/// A handle to one armed watcher, unique within a [`WatcherSet`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WatchId(pub u64);

/// What one scan of one row established.
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
    #[inline]
    fn from(matched: bool) -> Self {
        if matched {
            RowVerdict::Match
        } else {
            RowVerdict::NoMatch
        }
    }
}

/// An opaque, pre-compiled row matcher.
pub trait RowMatch: Send + Sync + std::fmt::Debug {
    /// What scanning `row` (one visible row's text) established.
    fn verdict(&self, row: &str) -> RowVerdict;

    /// The scan showed that `row` matches.
    #[inline]
    fn matches(&self, row: &str) -> bool {
        self.verdict(row) == RowVerdict::Match
    }

    /// The scan showed that `row` does not match.
    #[inline]
    fn clears(&self, row: &str) -> bool {
        self.verdict(row) == RowVerdict::NoMatch
    }
}

/// An inclusive range of **visible** row indices to match against, or every visible row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowRange {
    /// Every visible row (`0..rows`).
    All,
    /// The inclusive visible-row span `start..=end`, intersected with the grid rather than
    /// clamped.
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
    #[inline]
    const fn contains(self, idx: usize) -> bool {
        match self {
            RowRange::All => true,
            RowRange::Span { start, end } => idx >= start && idx <= end,
        }
    }

    /// Whether this range covers at least one of `visible_rows` grid rows.
    #[must_use]
    #[inline]
    pub const fn covers_any(self, visible_rows: usize) -> bool {
        self.on_grid(visible_rows).is_some()
    }

    /// The inclusive `(first, last)` of the rows this range covers on a grid of
    /// `visible_rows` rows (the range intersected with `0..visible_rows`), or
    /// `None` when it covers none.
    #[must_use]
    #[inline]
    const fn on_grid(self, visible_rows: usize) -> Option<(usize, usize)> {
        match self {
            RowRange::All => match visible_rows.checked_sub(1) {
                Some(last) => Some((0, last)),
                None => None,
            },
            RowRange::Span { start, end } => {
                if start <= end && start < visible_rows {
                    let last = if end < visible_rows - 1 {
                        end
                    } else {
                        visible_rows - 1
                    };
                    Some((start, last))
                } else {
                    None
                }
            }
        }
    }
}

/// The condition a watcher waits for.
#[derive(Clone, Debug)]
pub enum WatcherSpec {
    /// Latch once `content_seq()` exceeds `after`.
    SeqAdvanced {
        /// Latch once `content_seq()` exceeds this value.
        after: u64,
    },
    /// Latch after `dur` of no content mutation, at `activity_at + dur`.
    IdleFor {
        /// Latch after this much wall-time with no content mutation.
        dur: Duration,
    },
    /// Latch once `completed_command_seq()` exceeds `after`: a command finished (an OSC 133/633
    /// `D` the A→B→C→D gate accepted) since the count was read, whatever marks follow it in the
    /// same batch.
    BlockComplete {
        /// Latch once the completed-command count exceeds this value.
        after: u64,
    },
    /// Latch when any visible row in `rows` satisfies `matcher`.
    RowMatches {
        /// The opaque pre-compiled matcher.
        matcher: Arc<dyn RowMatch>,
        /// Which visible rows to scan.
        rows: RowRange,
    },
    /// Latch at `crossing`, when a decaying quantity crosses below `floor`.
    MomentumBelow {
        /// The floor the quantity must have decayed to.
        floor: f32,
        /// The instant the quantity reaches `floor` under the release law.
        crossing: Instant,
    },
    /// Latch when no visible row in `rows` satisfies `matcher` (the inverse of
    /// [`RowMatches`](Self::RowMatches)).
    RowsClear {
        /// The opaque pre-compiled matcher.
        matcher: Arc<dyn RowMatch>,
        /// Which visible rows to scan.
        rows: RowRange,
    },
    /// Latch when the program's frame is whole or [`FRAME_HANG`] fired (`await frame`).
    FrameWhole,
}

impl WatcherSpec {
    #[inline]
    const fn is_row(&self) -> bool {
        self.row_range().is_some()
    }

    /// The visible rows a row predicate reads; `None` for the scalar predicates.
    #[inline]
    const fn row_range(&self) -> Option<RowRange> {
        match self {
            WatcherSpec::RowMatches { rows, .. } | WatcherSpec::RowsClear { rows, .. } => {
                Some(*rows)
            }
            _ => None,
        }
    }

    /// Whether the predicate is level-triggered and evaluated once at arm.
    #[inline]
    const fn needs_arm_eval(&self) -> bool {
        matches!(
            self,
            WatcherSpec::RowMatches { .. }
                | WatcherSpec::RowsClear { .. }
                | WatcherSpec::SeqAdvanced { .. }
                | WatcherSpec::BlockComplete { .. }
                | WatcherSpec::FrameWhole
        )
    }

    /// Whether the predicate waits on a whole frame ([`WatcherSet::next_deadline`]).
    #[inline]
    const fn waits_on_frame(&self, deferred: bool) -> bool {
        matches!(self, WatcherSpec::FrameWhole) || (deferred && self.is_row())
    }
}

/// A latched satisfaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Satisfaction {
    /// The `content_seq` in force when the predicate became true.
    pub seq: u64,
    /// The instant the predicate became true (the batch instant or computed deadline).
    pub at: Instant,
}

/// The read-only surface a watcher evaluates against, implemented by [`Terminal`](super::Terminal).
pub trait SurfaceSource {
    /// The monotonic content clock (`content_seq`).
    fn content_seq(&self) -> u64;
    /// The monotonic count of completed shell commands (`completed_command_seq`).
    fn completed_command_seq(&self) -> u64;
    /// The number of visible rows.
    fn rows(&self) -> usize;
    /// The live screen's text of visible row `idx`, or `None`.
    fn row_text(&self, idx: usize) -> Option<String>;
}

/// The first visible row of `source` in `range` that `matcher` accepts.
#[inline]
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
/// `matcher`: the `RowMatches` scan.
#[inline]
#[must_use]
fn any_row_matches(rows: &[Option<String>], matcher: &dyn RowMatch, range: RowRange) -> bool {
    rows.iter().enumerate().any(|(idx, cell)| {
        range.contains(idx) && cell.as_deref().is_some_and(|t| matcher.matches(t))
    })
}

/// Whether every pre-collected visible row in `range` is shown not to match `matcher`: the
/// `RowsClear` scan.
#[inline]
#[must_use]
fn every_row_clear(rows: &[Option<String>], matcher: &dyn RowMatch, range: RowRange) -> bool {
    rows.iter().enumerate().all(|(idx, cell)| {
        !range.contains(idx) || cell.as_deref().is_none_or(|t| matcher.clears(t))
    })
}

/// The activity clock: the last `content_seq` seen to advance and the injected
/// instant at which it advanced.
#[derive(Clone, Copy, Debug, Default)]
struct ActivityClock {
    last_seq: u64,
    last_at: Option<Instant>,
    /// Which grid `last_seq` was read from (`content_seq` is per grid, so a flip resyncs).
    alt: bool,
    /// The screen generation `last_seq` was read in
    /// ([`invalidation_epoch`](super::ContentScrollState::invalidation_epoch)).
    epoch: u64,
}

/// One armed watcher — the single watcher type, distinguished only by `spec`.
#[derive(Clone)]
struct Watcher {
    id: WatchId,
    spec: WatcherSpec,
    /// For `IdleFor`, the current fire deadline (`activity_at + dur`), recomputed on each
    /// content advance; for `MomentumBelow`, the fixed crossing.
    deadline: Option<Instant>,
    /// `Some` once the predicate has held; sticky while armed.
    latched: Option<Satisfaction>,
    /// A freshly armed row watcher is scanned once even without a content advance, so an
    /// already-matching row (or already-clear range) latches at arm.
    fresh: bool,
    /// Whether a row scan was deferred by an unfinished frame until the next whole batch or hang
    /// detector.
    deferred: bool,
    /// `RowsClear` on a screen no closed frame vouches for: since when every scan has read
    /// the range clear.
    clear_since: Option<Instant>,
    /// Which grid this watcher was armed against; crossing a buffer swap latches `SeqAdvanced`.
    armed_alt: bool,
    /// Which screen generation this watcher was armed in
    /// ([`ActivityClock::epoch`]); crossing an epoch step latches too.
    armed_epoch: u64,
}

impl Watcher {
    /// When a settling `RowsClear` latches: [`GONE_SETTLE`] past `clear_since`
    /// (an unrepresentable instant reads as `clear_since`, fail open).
    #[inline]
    fn settled_at(&self) -> Option<Instant> {
        self.clear_since
            .map(|since| since.checked_add(GONE_SETTLE).unwrap_or(since))
    }
}

/// One whole-frame scan of a `RowsClear` that found its range
/// `clear` or not: whether it latches at this scan, and `clear_since` after it.
fn rows_clear_scan(
    clear: bool,
    at_arm: bool,
    frame: Frame,
    now: Instant,
    clear_since: &mut Option<Instant>,
) -> bool {
    if !clear {
        *clear_since = None;
        return false;
    }
    if frame.closed {
        return true;
    }
    let since = *clear_since.get_or_insert(if at_arm {
        frame.drawn_at.map_or(now, |d| d.min(now))
    } else {
        now
    });
    since.checked_add(GONE_SETTLE).is_none_or(|due| due <= now)
}

/// A bounded set of armed watchers plus the activity clock, held in [`Terminal`](super::Terminal).
#[derive(Clone)]
pub struct WatcherSet {
    watchers: Vec<Watcher>,
    clock: ActivityClock,
    next_id: u64,
    cap: usize,
    /// The unfinished frame's hang detector as the last observation saw it
    /// ([`super::Terminal::frame_hang_deadline`]); `None` when that frame was whole.
    frame_hang: Option<Instant>,
    /// The host read none of the output since [`Self::restart_idle`], so the hang detector is
    /// held.
    parked: bool,
}

/// Default capacity — bounds adversarial arming.
const DEFAULT_CAP: usize = 256;

impl Default for WatcherSet {
    #[inline]
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAP)
    }
}

impl WatcherSet {
    /// A set bounded to `cap` concurrently-armed watchers.
    #[inline]
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            watchers: Vec::new(),
            clock: ActivityClock::default(),
            next_id: 0,
            cap: cap.max(1),
            frame_hang: None,
            parked: false,
        }
    }

    /// `true` iff at least one watcher is armed.
    #[must_use]
    #[inline]
    pub const fn has_armed(&self) -> bool {
        !self.watchers.is_empty()
    }

    /// The screen generation the kernel last saw (`ActivityClock::epoch`).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn seen_epoch(&self) -> u64 {
        self.clock.epoch
    }

    /// Whether `(seq, alt, epoch)` is a surface change from what the kernel last saw.
    #[inline]
    #[must_use]
    pub const fn is_advance(&self, seq: u64, alt: bool, epoch: u64) -> bool {
        alt != self.clock.alt || epoch != self.clock.epoch || seq > self.clock.last_seq
    }

    /// Inclusive `(first, last)` visible rows a row scan reads this batch on a grid of `rows`
    /// rows across all un-latched watchers due for a scan.
    #[must_use]
    pub fn row_scan_hull(&self, advanced: bool, rows: usize) -> Option<(usize, usize)> {
        self.watchers
            .iter()
            .filter(|w| w.latched.is_none() && (advanced || w.fresh || w.deferred))
            .filter_map(|w| w.spec.row_range()?.on_grid(rows))
            .reduce(|(lo, hi), (first, last)| (lo.min(first), hi.max(last)))
    }

    /// Arm a watcher, returning its handle, or `None` (fail-closed) at capacity.
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
        let deadline = match &spec {
            WatcherSpec::IdleFor { dur } => Some(now + *dur),
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
            deferred: false,
            clear_since: None,
            armed_alt: self.clock.alt,
            armed_epoch: self.clock.epoch,
        });
        Some(id)
    }

    /// Remove a watcher (its observer went away).
    #[inline]
    pub fn disarm(&mut self, id: WatchId) {
        self.watchers.retain(|w| w.id != id);
    }

    /// Non-blocking: has `id` latched? `None` if pending or unknown.
    #[inline]
    #[must_use]
    pub fn poll(&self, id: WatchId) -> Option<Satisfaction> {
        self.watchers
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.latched)
    }

    /// Seed the activity baseline so the first `observe_screen` after arming does not read a
    /// phantom advance.
    #[inline]
    pub const fn seed_seq(&mut self, seq: u64) {
        if self.clock.last_seq < seq {
            self.clock.last_seq = seq;
        }
    }

    /// [`seed_seq`](Self::seed_seq) with the active-grid identity and screen generation.
    #[inline]
    pub const fn seed_screen(&mut self, seq: u64, alt: bool, epoch: u64) {
        if alt != self.clock.alt || epoch != self.clock.epoch {
            self.clock.alt = alt;
            self.clock.epoch = epoch;
            self.clock.last_seq = seq;
            return;
        }
        self.seed_seq(seq);
    }

    /// Soonest pending watcher deadline, settling `RowsClear` deadline, or unparked frame hang
    /// detector.
    #[inline]
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.watchers
            .iter()
            .filter(|w| w.latched.is_none())
            .filter_map(|w| {
                let hang = self
                    .frame_hang
                    .filter(|_| !self.parked && w.spec.waits_on_frame(w.deferred));
                [w.deadline, w.settled_at(), hang]
                    .into_iter()
                    .flatten()
                    .min()
            })
            .min()
    }

    /// Whether an unfinished frame's hang detector has fired by `now` with a watcher waiting on a
    /// whole frame.
    #[inline]
    #[must_use]
    pub fn frame_hang_due(&self, now: Instant) -> bool {
        self.frame_hang.is_some_and(|hang| now >= hang)
            && self
                .watchers
                .iter()
                .any(|w| w.latched.is_none() && w.spec.waits_on_frame(w.deferred))
    }

    /// [`observe_screen`](Self::observe_screen) on the main grid at the current generation.
    #[cfg(test)]
    pub fn observe(
        &mut self,
        content_seq: u64,
        completed_commands: u64,
        now: Instant,
        rows: &[Option<String>],
    ) -> bool {
        let epoch = self.clock.epoch;
        self.observe_screen(
            content_seq,
            false,
            epoch,
            completed_commands,
            now,
            rows,
            Frame::CLOSED,
        )
    }

    /// Run by `observe_at` after every batch (and at arm for a level-triggered spec).
    #[allow(
        clippy::too_many_arguments,
        reason = "one batch's facts, each read by the engine under its own borrow; a struct \
                  would only rename them at the one seam that passes them"
    )]
    pub fn observe_screen(
        &mut self,
        content_seq: u64,
        alt: bool,
        epoch: u64,
        completed_commands: u64,
        now: Instant,
        rows: &[Option<String>],
        frame: Frame,
    ) -> bool {
        self.frame_hang = frame.hang;
        let seq_before = self.clock.last_seq;
        let advanced = self.is_advance(content_seq, alt, epoch);
        if advanced {
            self.clock.last_seq = content_seq;
            self.clock.alt = alt;
            self.clock.epoch = epoch;
            self.clock.last_at = Some(now);
        }
        let here = Satisfaction {
            seq: content_seq,
            at: now,
        };
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            let mut new_latch: Option<Satisfaction> = None;
            let mut new_deadline: Option<Instant> = None;
            let mut deferred = w.deferred;
            let mut clear_since = w.clear_since;
            match &w.spec {
                WatcherSpec::SeqAdvanced { after } => {
                    // `after` is per grid: compare only within the same grid and
                    // generation; crossing a swap latches on the identity change.
                    if alt != w.armed_alt || epoch != w.armed_epoch || content_seq > *after {
                        new_latch = Some(here);
                    }
                }
                WatcherSpec::IdleFor { dur } => {
                    if advanced {
                        new_deadline = Some(now + *dur);
                    }
                }
                WatcherSpec::BlockComplete { after } => {
                    if completed_commands > *after {
                        new_latch = Some(here);
                    }
                }
                WatcherSpec::MomentumBelow { .. } => {}
                WatcherSpec::RowMatches { .. } | WatcherSpec::RowsClear { .. } if !frame.whole => {
                    if advanced || w.fresh {
                        deferred = true;
                        clear_since = None;
                    }
                }
                WatcherSpec::RowMatches {
                    matcher,
                    rows: range,
                } => {
                    if (advanced || w.fresh || w.deferred)
                        && any_row_matches(rows, &**matcher, *range)
                    {
                        new_latch = Some(here);
                    }
                    deferred = false;
                }
                WatcherSpec::RowsClear {
                    matcher,
                    rows: range,
                } => {
                    if let Some(due) = w.settled_at().filter(|&due| due <= now) {
                        new_latch = Some(Satisfaction {
                            seq: seq_before,
                            at: due,
                        });
                    } else if advanced || w.fresh || w.deferred {
                        let clear = range.covers_any(rows.len())
                            && every_row_clear(rows, &**matcher, *range);
                        let at_arm = w.fresh && !advanced;
                        new_latch = rows_clear_scan(clear, at_arm, frame, now, &mut clear_since)
                            .then_some(here);
                    }
                    deferred = false;
                }
                WatcherSpec::FrameWhole => {
                    if frame.whole {
                        new_latch = Some(here);
                    }
                }
            }
            w.fresh = false;
            w.deferred = deferred;
            w.clear_since = clear_since;
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

    /// Restart every un-latched `IdleFor` deadline at `now + dur` and hold the hang detector until
    /// the next [`Self::expire`].
    pub fn restart_idle(&mut self, now: Instant) {
        self.parked = true;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            if let WatcherSpec::IdleFor { dur } = w.spec {
                w.deadline = Some(now + dur);
            }
            if w.clear_since.is_some() {
                w.clear_since = Some(now);
            }
        }
    }

    /// Fire any deadlines reached by `now` and end the hang detector's hold
    /// ([`Self::restart_idle`]).
    pub fn expire(&mut self, now: Instant) -> bool {
        self.parked = false;
        let last_seq = self.clock.last_seq;
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            if let Some(due) = w.settled_at().filter(|&due| due <= now) {
                w.latched = Some(Satisfaction {
                    seq: last_seq,
                    at: due,
                });
                latched_any = true;
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
                            at: deadline,
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
    /// Run the observation kernel for the batch just processed (or at arm from
    /// [`watch`](Self::watch)).
    pub(super) fn observe_at(&mut self, now: Instant) -> bool {
        if !self.watchers.has_armed() {
            return false;
        }
        let hang = self.frame_hang_deadline().filter(|&hang| now < hang);
        let frame = Frame {
            whole: hang.is_none(),
            hang,
            closed: self.screen_is_closed_frame(),
            drawn_at: self.drawn_at(),
        };
        let seq = self.content_seq();
        let alt = self.is_alternate_screen();
        let epoch = self.content_scroll_state.invalidation_epoch;
        let advanced = self.watchers.is_advance(seq, alt, epoch);
        let completed = self.completed_command_seq();
        let rows = self.rows() as usize;
        let hull = if frame.whole {
            self.watchers.row_scan_hull(advanced, rows)
        } else {
            None
        };
        if let Some((lo, hi)) = hull {
            let mut scratch = std::mem::take(&mut self.row_text_scratch);
            scratch.resize_with(rows, || None);
            for (i, slot) in scratch.iter_mut().enumerate() {
                if i < lo || i > hi {
                    *slot = None;
                    continue;
                }
                let mut s = slot.take().unwrap_or_default();
                *slot = self.live_row_text_into(i, &mut s).then_some(s);
            }
            let latched = self
                .watchers
                .observe_screen(seq, alt, epoch, completed, now, &scratch, frame);
            self.row_text_scratch = scratch;
            latched
        } else {
            self.watchers
                .observe_screen(seq, alt, epoch, completed, now, &[], frame)
        }
    }

    /// Arm a surface watcher (called by the `await`/`subscribe` verbs).
    #[must_use]
    pub fn watch(&mut self, spec: WatcherSpec, now: Instant) -> Option<WatchId> {
        let arm_eval = spec.needs_arm_eval();
        let seq = self.content_seq();
        let epoch = self.content_scroll_state.invalidation_epoch;
        self.watchers
            .seed_screen(seq, self.is_alternate_screen(), epoch);
        let id = self.watchers.arm(spec, now)?;
        if arm_eval {
            let _ = self.observe_at(now);
        }
        Some(id)
    }

    /// Latch when any visible row in `rows` matches `matcher`.
    #[inline]
    #[must_use]
    pub fn watch_rows(
        &mut self,
        matcher: Arc<dyn RowMatch>,
        rows: RowRange,
        now: Instant,
    ) -> Option<WatchId> {
        self.watch(WatcherSpec::RowMatches { matcher, rows }, now)
    }

    /// Inverse of [`watch_rows`](Self::watch_rows): latch when no visible row in `rows` matches
    /// `matcher` (`await gone`).
    #[inline]
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
    #[inline]
    #[must_use]
    pub fn watch_poll(&self, id: WatchId) -> Option<Satisfaction> {
        self.watchers.poll(id)
    }

    /// Remove a watcher.
    #[inline]
    pub fn watch_disarm(&mut self, id: WatchId) {
        self.watchers.disarm(id);
    }

    /// The soonest pending deadline ([`WatcherSet::next_deadline`]).
    #[inline]
    #[must_use]
    pub fn watch_next_deadline(&self) -> Option<Instant> {
        self.watchers.next_deadline()
    }

    /// Fire host-driven deadlines ([`WatcherSet::expire`]) and any due frame hang detector
    /// ([`WatcherSet::frame_hang_due`]).
    #[inline]
    pub fn watch_expire(&mut self, now: Instant) -> bool {
        let expired = self.watchers.expire(now);
        let hung = self.watchers.frame_hang_due(now) && self.observe_at(now);
        expired || hung
    }

    /// Restart every pending `IdleFor` at `now` and hold the hang detector
    /// ([`WatcherSet::restart_idle`]).
    #[inline]
    pub fn watch_restart_idle(&mut self, now: Instant) {
        self.watchers.restart_idle(now);
    }

    /// `true` iff any watcher is armed (the producer's wake fan-out gate).
    #[inline]
    #[must_use]
    pub const fn watchers_armed(&self) -> bool {
        self.watchers.has_armed()
    }
}

/// The local engine is a [`SurfaceSource`].
impl SurfaceSource for super::Terminal {
    #[inline]
    fn content_seq(&self) -> u64 {
        self.content_seq()
    }
    #[inline]
    fn completed_command_seq(&self) -> u64 {
        self.completed_command_seq()
    }
    #[inline]
    fn rows(&self) -> usize {
        self.rows() as usize
    }
    #[inline]
    fn row_text(&self, idx: usize) -> Option<String> {
        let mut s = String::new();
        self.live_row_text_into(idx, &mut s).then_some(s)
    }
}

impl super::Terminal {
    /// Write live screen row `idx`'s text into `out` (cleared first), ignoring scroll offset.
    #[inline]
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
#[path = "observe_tests.rs"]
mod tests;
