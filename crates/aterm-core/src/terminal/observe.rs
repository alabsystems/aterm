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
//! 3. Whole frames only: the row predicates (`RowMatches`, `RowsClear`) never read the rows of a
//!    frame the program has not finished. A program that brackets its redraws in DEC 2026
//!    (synchronized output) and has one open, written into, leaves a prefix of that frame on the
//!    grid when a burst of the PTY reader ends inside it: Claude Code 2.1.308 and agy 1.4.0 do (the lab's casts,
//!    `docs/measured/half-drawn-frame-2026-10-22.md`), and a busy footer the frame has not
//!    reached yet reads gone. The scan a batch inside such a frame owes is held until the first
//!    batch that leaves the frame whole (its `?2026l`, which may carry no content), or until the
//!    run of batches ending inside frames has lasted [`FRAME_HANG`] (a hang detector: a program
//!    that never closes its frame, or whose every read ends inside its next one, cannot freeze a
//!    wait, [`WatcherSet::next_deadline`] wakes it there). `FrameWhole` (`await frame`) waits for
//!    exactly that.
//! 4. Gone only on a settled screen: `RowsClear` (`await gone`) latches at once only on a screen
//!    that is a frame the program closed with nothing drawn since
//!    ([`Frame::closed`](Frame::closed)). Any other screen may be a redraw the PTY's reads split
//!    with no bracket to say so (Gemini CLI 0.46.0 clears and redraws its whole screen per
//!    frame, and a read that ends partway shows no spinner), so there the range must read clear
//!    at every scan for [`GONE_SETTLE`], measured from the batch that first showed it clear (from
//!    the screen's last change, [`Frame::drawn_at`], for one clear at arm), and latches at that
//!    computed deadline, however late it is observed.
//!
//! The kernel only observes: it never mutates the surface and is never checkpointed, so it
//! cannot affect replay. Its clock is the injected
//! [`process_now`](super::TransientState::process_now), never the wall clock.

use std::sync::Arc;
use std::time::Duration;

use aterm_time::Instant;

/// How long a reader of whole frames waits on a program's unfinished DEC 2026 frame before it
/// reads the frame as it stands ([`super::Terminal::frame_unfinished_at`]): a hang detector for a
/// program that opened a frame and never closed it, never a latency budget. Counted from the
/// first batch of a run that ended inside a frame, not from each frame's first write, so a
/// program whose every read ends inside its next frame reaches it too. A live program closes
/// its frame within milliseconds (the measured splits close 0.1 to 1.3 ms after they open).
pub const FRAME_HANG: Duration = Duration::from_secs(60);

/// How long a range must read clear before `RowsClear` (`await gone`) latches on a screen
/// that is not a frame the program closed (module doc, property 4): a redraw the PTY's reads
/// split, with no DEC 2026 bracket to say so, is finished by a later read, inside it. MEASURED
/// (`docs/measured/half-drawn-frame-2026-10-22.md`): Gemini CLI 0.46.0, three tabs at once in
/// the lab, finishes a split redraw a median 3 ms after its first part, but under that load the
/// rest of one came as late as 385 ms (929 split redraws, 11 of them past 100 ms, which a
/// 100 ms settle latched on: 6 false `gone`s in 33 replies). A second is that tail with a margin
/// of more than two and a half; a closed frame (Claude Code's, agy's) latches with no settle.
pub const GONE_SETTLE: Duration = Duration::from_secs(1);

/// Whether the program's frame is whole at one observation pass, and when its hang
/// detector fires if it is not ([`WatcherSet::observe_screen`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// No row predicate may read the rows when `false` (module doc, property 3).
    pub whole: bool,
    /// The unfinished frame's hang detector ([`FRAME_HANG`] past the first batch of the
    /// run that ended inside a frame); `None` on a whole frame.
    pub hang: Option<Instant>,
    /// The screen is a frame the program closed with nothing drawn since
    /// ([`super::Terminal::screen_is_closed_frame`]): whole by the program's own word, so
    /// `RowsClear` latches on it at once (module doc, property 4).
    pub closed: bool,
    /// When the screen last changed ([`super::Terminal::drawn_at`]): a range clear at arm
    /// has been clear since then, and its [`GONE_SETTLE`] runs from there. `None`: from the
    /// pass's own instant.
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

    /// A whole frame no bracket vouches for (a program that brackets nothing, or output
    /// after its last close), drawn at `drawn_at`: `RowsClear` settles on it.
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
    #[inline]
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
    #[inline]
    fn matches(&self, row: &str) -> bool {
        self.verdict(row) == RowVerdict::Match
    }

    /// The scan showed that `row` does not match. An undecided row does not.
    #[inline]
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
    #[inline]
    const fn contains(self, idx: usize) -> bool {
        match self {
            RowRange::All => true,
            RowRange::Span { start, end } => idx >= start && idx <= end,
        }
    }

    /// Whether this range covers at least one of `visible_rows` grid rows. An
    /// empty range scans nothing, which is never evidence of a clear surface,
    /// so `RowsClear` will not latch on it.
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
    /// Latch when the program's frame is whole: no DEC 2026 frame it opened and wrote into is
    /// still open, or the open one's hang detector ([`FRAME_HANG`]) fired (`await frame`). What a
    /// reader that found a frame unfinished (`text --json`'s `"frame":"open"`) waits on before it
    /// reads again. Level-triggered: a whole frame at arm latches at arm.
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

    /// Whether the predicate is level-triggered and so evaluated once at arm:
    /// an already-matching row (`RowMatches`), an already-clear range
    /// (`RowsClear`) or an already-advanced `content_seq` (`SeqAdvanced`, an
    /// agent's "did anything change since my seq?") must answer yes without
    /// waiting for another batch.
    ///
    /// `IdleFor` is arm-relative (see [`WatcherSet::arm`]), and `BlockComplete`
    /// latching at arm would fire `await block` on the previous block.
    #[inline]
    const fn needs_arm_eval(&self) -> bool {
        matches!(
            self,
            WatcherSpec::RowMatches { .. }
                | WatcherSpec::RowsClear { .. }
                | WatcherSpec::SeqAdvanced { .. }
                | WatcherSpec::FrameWhole
        )
    }

    /// Whether the predicate waits on a whole frame, so an unfinished frame's hang detector is
    /// a deadline it is woken at ([`WatcherSet::next_deadline`]): `FrameWhole`, and a row
    /// predicate whose scan an unfinished frame held ([`Watcher::deferred`]).
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
    /// The instant the predicate became true: the processed-batch instant for
    /// `SeqAdvanced`/`BlockComplete`/`RowMatches`/`RowsClear`/`FrameWhole` (the expiry
    /// instant where an unfinished frame's hang detector made the frame whole), the computed
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
/// `matcher`: the `RowMatches` scan. Stops at the first hit.
#[inline]
#[must_use]
fn any_row_matches(rows: &[Option<String>], matcher: &dyn RowMatch, range: RowRange) -> bool {
    rows.iter().enumerate().any(|(idx, cell)| {
        range.contains(idx) && cell.as_deref().is_some_and(|t| matcher.matches(t))
    })
}

/// Whether every pre-collected visible row in `range` is shown not to match `matcher`: the
/// `RowsClear` scan. A row the matcher could not decide keeps the range from reading clear (see
/// [`RowMatch`]); a row with no text has nothing to match. Stops at the first row not clear.
#[inline]
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
    /// A row scan this watcher was owed (an advance, or `fresh`) was held because the program's
    /// frame was unfinished (module doc, property 3): it is owed at the first whole batch, which
    /// may carry no content (a lone `?2026l`), or at the frame's hang detector.
    deferred: bool,
    /// `RowsClear` on a screen no closed frame vouches for: since when every scan has read
    /// the range clear (module doc, property 4). It latches at this plus [`GONE_SETTLE`];
    /// a scan that reads a row in range not clear, or a batch whose scan an unfinished
    /// frame held, clears it. `None` for the other specs.
    clear_since: Option<Instant>,
    /// Which grid this watcher was armed against: `SeqAdvanced`'s `after` is per grid, so
    /// crossing a buffer swap latches; otherwise `await seq` would starve on the alt screen and
    /// fire spuriously on 1049 exit.
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

/// One whole-frame scan of a `RowsClear` (module doc, property 4) that found its range
/// `clear` or not: whether it latches at this scan, and `clear_since` after it. A frame the program closed is
/// whole by its own word and latches at once; any other clear scan starts or continues the
/// settle, since this scan or, at arm (`at_arm`: nothing drawn since the arm) since the screen
/// last changed, and latches once it has lasted [`GONE_SETTLE`].
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

/// A bounded set of armed watchers plus the activity clock, held in
/// [`Terminal`](super::Terminal): never checkpointed, never mutates the surface.
#[derive(Clone)]
pub struct WatcherSet {
    watchers: Vec<Watcher>,
    clock: ActivityClock,
    next_id: u64,
    cap: usize,
    /// The unfinished frame's hang detector as the last observation saw it
    /// ([`super::Terminal::frame_hang_deadline`]); `None` when that frame was whole.
    frame_hang: Option<Instant>,
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
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn seen_epoch(&self) -> u64 {
        self.clock.epoch
    }

    /// Whether `(seq, alt, epoch)` is a surface change from what the kernel last
    /// saw: another grid, another generation, or a higher seq on the same one.
    /// The one change test, shared by the row-scan gate and
    /// [`observe_screen`](Self::observe_screen).
    #[inline]
    #[must_use]
    pub const fn is_advance(&self, seq: u64, alt: bool, epoch: u64) -> bool {
        alt != self.clock.alt || epoch != self.clock.epoch || seq > self.clock.last_seq
    }

    /// The inclusive `(first, last)` visible rows a row scan reads this batch on a
    /// grid of `rows` rows: the hull of the on-grid ranges of every un-latched row
    /// watcher that is fresh, sees content advance or is owed a scan an unfinished
    /// frame held, exactly the watchers [`observe_screen`](Self::observe_screen)
    /// scans on a whole frame. `None` when none of them covers a row, so the scan
    /// reads nothing. The engine collects row text only inside the hull: every row
    /// a scanning watcher reads lies in it.
    #[must_use]
    pub fn row_scan_hull(&self, advanced: bool, rows: usize) -> Option<(usize, usize)> {
        self.watchers
            .iter()
            .filter(|w| w.latched.is_none() && (advanced || w.fresh || w.deferred))
            .filter_map(|w| w.spec.row_range()?.on_grid(rows))
            .reduce(|(lo, hi), (first, last)| (lo.min(first), hi.max(last)))
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
            deferred: false,
            clear_since: None,
            // The clock's grid identity is the arming grid: `Terminal::watch`
            // seeds it (`seed_screen`) immediately before arming.
            armed_alt: self.clock.alt,
            armed_epoch: self.clock.epoch,
        });
        Some(id)
    }

    /// Remove a watcher (its observer went away). Idempotent.
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

    /// Seed the activity baseline so the first `observe_screen` after arming does not
    /// read a phantom advance. Monotone: never lowers an existing baseline.
    #[inline]
    pub const fn seed_seq(&mut self, seq: u64) {
        if self.clock.last_seq < seq {
            self.clock.last_seq = seq;
        }
    }

    /// [`seed_seq`](Self::seed_seq) with the active-grid identity and screen
    /// generation: a new grid or generation sets the baseline rather than
    /// raising it, since monotonicity only holds within one generation of one
    /// buffer.
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

    /// Whether an un-latched `BlockComplete` watcher is armed; gates the
    /// O(blocks) shell-integration walk in `observe_at`.
    #[inline]
    #[must_use]
    pub fn has_block_complete(&self) -> bool {
        self.watchers
            .iter()
            .any(|w| w.latched.is_none() && matches!(w.spec, WatcherSpec::BlockComplete))
    }

    /// The soonest pending `IdleFor` / `MomentumBelow` deadline, a settling
    /// `RowsClear`'s ([`GONE_SETTLE`] past its `clear_since`), and the
    /// unfinished frame's hang detector where a watcher waits on a whole frame
    /// ([`WatcherSpec::FrameWhole`], a row scan the frame held), which the
    /// arming `await`/`ready` verb uses to bound its `Subscription::wait` park.
    /// `None` when no un-latched deadline watcher is armed.
    #[inline]
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.watchers
            .iter()
            .filter(|w| w.latched.is_none())
            .filter_map(|w| {
                let hang = self
                    .frame_hang
                    .filter(|_| w.spec.waits_on_frame(w.deferred));
                [w.deadline, w.settled_at(), hang]
                    .into_iter()
                    .flatten()
                    .min()
            })
            .min()
    }

    /// Whether an unfinished frame's hang detector has fired by `now` with a
    /// watcher still waiting on a whole frame: the host's expiry pass then runs
    /// the observation again ([`super::Terminal::watch_expire`]), and that pass
    /// reads the frame as it stands.
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
        newest_block_complete: bool,
        now: Instant,
        rows: &[Option<String>],
    ) -> bool {
        let epoch = self.clock.epoch;
        self.observe_screen(
            content_seq,
            false,
            epoch,
            newest_block_complete,
            now,
            rows,
            Frame::CLOSED,
        )
    }

    /// The seam call, run by `observe_at` after every batch (and at arm for a level-triggered
    /// spec) with `now == transient.process_now`. Stamps activity if the surface advanced and
    /// latches every predicate that holds, in one pass. A change of grid or generation is a
    /// resync: the baseline is set, not raised, since the counter is not comparable across it
    /// (`ActivityClock::epoch`). `rows[idx]` is visible-row text for the row predicates; the
    /// caller collects it only inside [`row_scan_hull`](Self::row_scan_hull) (`None` outside,
    /// `rows` still one entry per grid row), and an empty `rows` is never read as a clear
    /// surface. `frame` says whether the program's frame is whole at this pass: on an
    /// unfinished one no row predicate reads `rows` (module doc, property 3), and the
    /// scan each was owed is held for the first whole pass. Returns `true` if anything
    /// latched.
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
        newest_block_complete: bool,
        now: Instant,
        rows: &[Option<String>],
        frame: Frame,
    ) -> bool {
        self.frame_hang = frame.hang;
        // The seq a settle that ran out before this batch was in force at.
        let seq_before = self.clock.last_seq;
        let advanced = self.is_advance(content_seq, alt, epoch);
        if advanced {
            self.clock.last_seq = content_seq;
            self.clock.alt = alt;
            self.clock.epoch = epoch;
            self.clock.last_at = Some(now);
        }
        // A latch at this batch.
        let here = Satisfaction {
            seq: content_seq,
            at: now,
        };
        let mut latched_any = false;
        for w in &mut self.watchers {
            if w.latched.is_some() {
                continue;
            }
            // Decide the latch / deadline update with the spec borrow, then apply
            // the field writes after the match (disjoint-borrow safe).
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
                WatcherSpec::BlockComplete => {
                    if newest_block_complete {
                        new_latch = Some(here);
                    }
                }
                // Content activity is not this predicate's business: the
                // deadline was fixed at arm and fires from `expire` alone.
                WatcherSpec::MomentumBelow { .. } => {}
                // Dirty-row gate: scan only on advance, a fresh arm, or a scan an unfinished
                // frame held; and never inside an unfinished frame, whose rows are a prefix
                // of a redraw (module doc, property 3): the scan is held for a whole one.
                WatcherSpec::RowMatches { .. } | WatcherSpec::RowsClear { .. } if !frame.whole => {
                    if advanced || w.fresh {
                        deferred = true;
                        // What the frame drew is unread: no evidence the range stayed clear.
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
                        // The range read clear for GONE_SETTLE before this batch: it latched
                        // then, at the computed deadline, whatever this batch drew (property 1).
                        new_latch = Some(Satisfaction {
                            seq: seq_before,
                            at: due,
                        });
                    } else if advanced || w.fresh || w.deferred {
                        // Clear only when every row in range is shown not to match, on a batch
                        // with rows collected and a range meeting at least one: scanning
                        // nothing, or a row the matcher could not decide, is not evidence of a
                        // clear surface.
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
            // A settling `RowsClear` too: the screen it read clear may be stale.
            if w.clear_since.is_some() {
                w.clear_since = Some(now);
            }
        }
    }

    /// The idle-fire call, run when the host's `WaitUntil` wake reaches `now`
    /// (and once at a replay target). Latches every un-latched `IdleFor` /
    /// `MomentumBelow` whose deadline has passed, and every `RowsClear` whose
    /// range has read clear for [`GONE_SETTLE`], with `at = deadline`, so the
    /// result does not depend on when the host woke. Returns `true` if anything
    /// latched.
    pub fn expire(&mut self, now: Instant) -> bool {
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
    pub(super) fn observe_at(&mut self, now: Instant) -> bool {
        if !self.watchers.has_armed() {
            return false;
        }
        // A frame the program has not finished is no frame to read rows from (module doc,
        // property 3); past its hang detector it is read as it stands.
        let hang = self.frame_hang_deadline().filter(|&hang| now < hang);
        let frame = Frame {
            whole: hang.is_none(),
            hang,
            closed: self.screen_is_closed_frame(),
            drawn_at: self.drawn_at(),
        };
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
        // Collect visible-row text only when a row scan will run, and only for the rows a
        // scanning watcher can read, into a persistent scratch taken out of `self` (row reads
        // borrow `&self` while `observe_screen` borrows `self.watchers`); the `Vec` and each
        // kept `String`'s capacity are reused across batches. The scratch keeps one entry per
        // grid row, so every scanning watcher sees exactly what a full collection gives it.
        // The live screen's rows, as `text` serves them: a person scrolled back must not make
        // a live box look gone, nor an old copy of it in history look live.
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
            let latched = self.watchers.observe_screen(
                seq,
                alt,
                epoch,
                newest_complete,
                now,
                &scratch,
                frame,
            );
            self.row_text_scratch = scratch;
            latched
        } else {
            // Gate closed (or the frame unfinished): no row text needed this batch.
            self.watchers
                .observe_screen(seq, alt, epoch, newest_complete, now, &[], frame)
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
            let _ = self.observe_at(now);
        }
        Some(id)
    }

    /// Convenience for the common row predicate: latch when any visible row in
    /// `rows` matches `matcher` (built in `aterm-observe`).
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

    /// The inverse of [`watch_rows`](Self::watch_rows): latch when no visible row
    /// in `rows` matches `matcher` (`await gone`). Level-triggered.
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

    /// Remove a watcher. Idempotent.
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

    /// Host-driven deadline firing ([`WatcherSet::expire`]), and the unfinished
    /// frame's hang detector ([`WatcherSet::frame_hang_due`]): once it fires, the
    /// observation runs again at `now`, which reads the frame as it stands, so a
    /// program that never closes its frame cannot hold a wait on a whole one.
    #[inline]
    pub fn watch_expire(&mut self, now: Instant) -> bool {
        let expired = self.watchers.expire(now);
        let hung = self.watchers.frame_hang_due(now) && self.observe_at(now);
        expired || hung
    }

    /// Idle hold: while the host consumes none of this terminal's output,
    /// restart every pending `IdleFor` at `now` ([`WatcherSet::restart_idle`]).
    #[inline]
    pub fn watch_restart_idle(&mut self, now: Instant) {
        self.watchers.restart_idle(now);
    }

    /// `true` iff any watcher is armed (the producer's wake fan-out gate).
    #[inline]
    #[must_use]
    pub fn watchers_armed(&self) -> bool {
        self.watchers.has_armed()
    }
}

/// The local engine is a [`SurfaceSource`]. (Inherent methods shadow the trait
/// methods, so these do not recurse.)
impl SurfaceSource for super::Terminal {
    #[inline]
    fn content_seq(&self) -> u64 {
        self.content_seq()
    }
    #[inline]
    fn newest_block_complete(&self) -> bool {
        self.all_blocks().last().is_some_and(|b| {
            matches!(
                b.state,
                super::BlockState::PromptOnly | super::BlockState::Complete
            )
        })
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
    /// Live screen row `idx`'s text into `out` (cleared first), whatever the display
    /// offset: the frame the control protocol's `text` and the input fence's `if-fp=`
    /// read, where [`Terminal::row_text_into`](super::Terminal::row_text_into) follows
    /// the person's scroll. `false` for a row past the screen.
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
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now() // test seed; all deltas below are explicit
    }

    /// No row text: the scalar predicates do not read rows.
    const NO_ROWS: &[Option<String>] = &[];

    /// Matches a row containing the literal.
    #[derive(Debug)]
    struct Contains(&'static str);
    impl RowMatch for Contains {
        fn verdict(&self, row: &str) -> RowVerdict {
            row.contains(self.0).into()
        }
    }

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
        assert!(
            WatcherSpec::FrameWhole.needs_arm_eval(),
            "FrameWhole is level-triggered: a whole frame at arm latches at arm"
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
            w.observe_screen(
                15,
                true,
                1,
                false,
                base,
                &[Some("ls".to_string())],
                Frame::CLOSED,
            );
            assert!(w.poll(rows).is_none() && w.poll(seq).is_none());
            assert_eq!(w.is_advance(15, true, epoch), moves, "epoch {epoch}");
            w.observe_screen(15, true, epoch, false, base, &box_b, Frame::CLOSED);
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

    /// `RowsClear` on a screen no closed frame vouches for (module doc, property 4): the range
    /// must read clear at every scan for GONE_SETTLE, measured from the first clear scan; a
    /// scan showing the row again cancels it; the host's expiry latches at the computed
    /// deadline; `restart_idle` (the reader parked for an update) restarts it. NEGATIVE
    /// CONTROL: on a closed frame the same scan latches at once.
    #[test]
    fn rows_clear_settles_on_a_screen_no_closed_frame_vouches_for() {
        let base = t0();
        let ms = |n| base + Duration::from_millis(n);
        let busy = [Some("esc to interrupt".to_string())];
        let clear = [Some("Done.".to_string())];
        let unb = |n| Frame::unbracketed(Some(ms(n)));
        let mut w = WatcherSet::default();
        let id = w.arm(rows_clear(RowRange::All), base).unwrap();
        w.observe_screen(1, false, 0, false, ms(0), &busy, unb(0));
        w.observe_screen(2, false, 0, false, ms(10), &clear, unb(10));
        assert!(w.poll(id).is_none());
        assert_eq!(w.next_deadline(), Some(ms(10) + GONE_SETTLE));
        // Still clear at a later scan: the settle runs from the first.
        w.observe_screen(3, false, 0, false, ms(50), &clear, unb(50));
        assert_eq!(w.next_deadline(), Some(ms(10) + GONE_SETTLE));
        // The row back cancels it.
        w.observe_screen(4, false, 0, false, ms(60), &busy, unb(60));
        assert_eq!(w.next_deadline(), None);
        assert!(!w.expire(ms(10) + GONE_SETTLE));
        w.observe_screen(5, false, 0, false, ms(200), &clear, unb(200));
        // Parked: unseen time is not quiet time.
        w.restart_idle(ms(250));
        assert_eq!(w.next_deadline(), Some(ms(250) + GONE_SETTLE));
        assert!(!w.expire(ms(200) + GONE_SETTLE));
        assert!(w.expire(ms(250) + GONE_SETTLE));
        assert_eq!(
            w.poll(id),
            Some(Satisfaction {
                seq: 5,
                at: ms(250) + GONE_SETTLE
            })
        );
        // NEGATIVE CONTROL: a closed frame is believed at once.
        let closed = w.arm(rows_clear(RowRange::All), ms(300)).unwrap();
        w.observe_screen(6, false, 0, false, ms(300), &clear, Frame::CLOSED);
        assert_eq!(
            w.poll(closed),
            Some(Satisfaction {
                seq: 6,
                at: ms(300)
            })
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

    /// The hull is taken over exactly the watchers `observe_screen` scans, each range
    /// intersected with the grid.
    #[test]
    fn row_scan_hull_covers_the_scanning_watchers_on_the_grid() {
        let base = t0();
        let span = |start, end| RowRange::Span { start, end };
        let shown = |range| WatcherSpec::RowMatches {
            matcher: Arc::new(Contains("never shown")),
            rows: range,
        };
        let mut w = WatcherSet::default();
        assert_eq!(w.row_scan_hull(true, 24), None, "no watcher, no scan");
        let scalar = w.arm(WatcherSpec::SeqAdvanced { after: 0 }, base).unwrap();
        assert_eq!(
            w.row_scan_hull(true, 24),
            None,
            "a scalar watcher reads no row"
        );
        w.disarm(scalar);
        // Ranges that meet no row of a 24-row grid contribute nothing.
        w.arm(shown(span(40, 50)), base).unwrap();
        w.arm(rows_clear(span(2, 1)), base).unwrap();
        assert_eq!(w.row_scan_hull(true, 24), None);
        w.arm(shown(span(20, 20)), base).unwrap();
        w.arm(rows_clear(span(5, 99)), base).unwrap();
        assert_eq!(
            w.row_scan_hull(true, 24),
            Some((5, 23)),
            "rows 5..=99 is cut at the last grid row"
        );
        w.arm(shown(span(0, 0)), base).unwrap();
        assert_eq!(w.row_scan_hull(true, 24), Some((0, 23)));
        assert_eq!(
            w.row_scan_hull(true, 0),
            None,
            "a grid with no rows has none to read"
        );
        // Once scanned (busy everywhere, so nothing latches), a watcher is no longer fresh:
        // it reads rows only on an advance.
        let busy = vec![Some("esc to interrupt".to_string()); 24];
        w.observe(0, false, base, &busy);
        assert_eq!(w.row_scan_hull(false, 24), None);
        assert_eq!(w.row_scan_hull(true, 24), Some((0, 23)));
        // A fresh arm on a quiet surface reads its own range alone; the others wait for an
        // advance.
        w.arm(rows_clear(RowRange::All), base).unwrap();
        assert_eq!(w.row_scan_hull(false, 3), Some((0, 2)));
        // A latched watcher reads nothing.
        let mut once = WatcherSet::default();
        let id = once.arm(shown(RowRange::All), base).unwrap();
        once.observe(1, false, base, &[Some("never shown".to_string())]);
        assert!(once.poll(id).is_some());
        assert_eq!(once.row_scan_hull(true, 24), None);
    }

    /// `observe_at` renders only the armed rows' hull, and the scratch keeps one entry per
    /// grid row.
    #[test]
    fn observe_at_renders_only_the_armed_span() {
        use crate::terminal::Terminal;

        let rendered = |t: &Terminal| -> Vec<usize> {
            t.row_text_scratch
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.is_some().then_some(i))
                .collect()
        };
        let now = t0();
        let mut term = Terminal::new(24, 40);
        let top = term
            .watch_rows(
                Arc::new(Contains("never shown")),
                RowRange::Span { start: 0, end: 0 },
                now,
            )
            .expect("arm rows 0..=0");
        term.process(b"one\r\n");
        assert_eq!(term.row_text_scratch.len(), 24, "one entry per grid row");
        assert_eq!(
            rendered(&term),
            [0],
            "a rows 0..=0 watcher reads row 0 alone"
        );
        // NEGATIVE CONTROL: an `All` watcher on the same batch shape reads every row.
        let all = term
            .watch_rows(Arc::new(Contains("never shown")), RowRange::All, now)
            .expect("arm all rows");
        term.process(b"two\r\n");
        assert_eq!(rendered(&term), (0..24).collect::<Vec<_>>());
        // Disarmed, the hull narrows again and keeps no text from the rows it left.
        term.watch_disarm(all);
        term.process(b"three\r\n");
        assert_eq!(rendered(&term), [0]);
        assert!(term.watch_poll(top).is_none());
    }

    /// Watchers on disjoint spans latch at the same seqs whether the scan reads only their
    /// hull or, with an extra `All` watcher armed, every row. The batches are a second apart,
    /// and each is followed by the host's expiry pass `GONE_SETTLE` after it.
    #[test]
    fn disjoint_spans_latch_exactly_as_a_full_scan() {
        use crate::terminal::{ClockReading, Terminal};

        // Row 5 is busy before the arm; row 23 shows READY outside every range.
        const SETUP: &[u8] = b"\x1b[6;1Hbusy\x1b[24;1HREADY";
        const CORPUS: [&[u8]; 6] = [
            b"\x1b[22;1HREADY",     // row 21: outside every range
            b"\x1b[21;1HREADY",     // row 20: the second span latches
            b"\x1b[7;1Hstill busy", // row 6: rows 5..=7 stay busy
            b"\x1b[6;1H\x1b[2K",    // row 5 clears, row 6 is still busy
            b"\x1b[7;1H\x1b[2K",    // row 6 clears: rows 5..=7 are clear
            b"\x1b[1;1HREADY",      // row 0: the first span latches
        ];
        let run = |full_scan: bool| {
            let now = t0();
            let at = |i: u64| ClockReading {
                monotonic: now + Duration::from_secs(i),
                wall_ms: None,
            };
            let mut term = Terminal::new(24, 40);
            term.process_at(SETUP, at(0));
            if full_scan {
                term.watch_rows(Arc::new(Contains("never shown")), RowRange::All, now)
                    .expect("arm the full-scan watcher");
            }
            let ids = [
                term.watch_rows(
                    Arc::new(Contains("READY")),
                    RowRange::Span { start: 0, end: 0 },
                    now,
                ),
                term.watch_rows(
                    Arc::new(Contains("READY")),
                    RowRange::Span { start: 20, end: 20 },
                    now,
                ),
                term.watch_rows_gone(
                    Arc::new(Contains("busy")),
                    RowRange::Span { start: 5, end: 7 },
                    now,
                ),
            ]
            .map(|id| id.expect("arm"));
            let mut latches = Vec::new();
            let mut read_last_row = false;
            for (i, batch) in (1..).zip(CORPUS) {
                term.process_at(batch, at(i));
                term.watch_expire(at(i).monotonic + GONE_SETTLE);
                read_last_row |= term.row_text_scratch.get(23).is_some_and(Option::is_some);
                latches.push(ids.map(|id| term.watch_poll(id).map(|s| s.seq)));
            }
            (latches, read_last_row)
        };
        let (hull, hull_read_last) = run(false);
        let (full, full_read_last) = run(true);
        // NEGATIVE CONTROL: the two runs really read different rows, so their agreement is
        // not two full scans agreeing.
        assert!(full_read_last, "the full scan reads row 23");
        assert!(
            !hull_read_last,
            "the hull of 0..=0, 20..=20 and 5..=7 never reads row 23"
        );
        assert_eq!(hull, full, "the hull latches exactly as the full scan");
        // Each watcher latches on its own batch, and `gone` not before rows 5..=7 clear.
        let first_latch: Vec<Option<usize>> = (0..3)
            .map(|w| hull.iter().position(|step| step[w].is_some()))
            .collect();
        assert_eq!(first_latch, [Some(5), Some(1), Some(4)]);
    }
}
