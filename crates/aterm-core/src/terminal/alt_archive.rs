// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The alt-screen scroll-off archive: the rows a fullscreen app scrolled away.
//!
//! A fullscreen app on the alternate screen often repaints with absolute cursor
//! moves and erases inside DEC 2026 synchronized updates and emits no scroll
//! sequence at all. The alt grid has no scrollback, so a row that leaves the top
//! is gone. This archive keeps those rows.
//!
//! [`AltArchive`] is a pure differ and store over committed frames (row text plus
//! a u64 row hash, no `Terminal` coupling). Between two committed frames it finds
//! how far the content moved by anchor voting (rows distinctive and unique in both
//! frames vote for the offset mapping one onto the other) and archives the rows
//! that left the top. Where it cannot tell, it prefers a duplicate to a loss: rows
//! that were displayed are flushed before a discontinuity, and every discontinuity
//! is recorded as a gap a reader can see.
//!
//! The `Terminal` hook: `process_at` splits its input after every `CSI ? … 2026 … l`
//! (ESU) with a streaming matcher that survives a sequence straddling two PTY
//! reads, and commits the alt grid when `sync_end_seq` moved. The matcher stops one
//! byte short of an alt-screen exit so the alt grid is committed before the swap.
//! Apps that never use 2026 are committed at the batch epilogue, rate-limited.
//!
//! The archive holds only rows `text` already showed, lives in memory, is never
//! part of a checkpoint, is wiped by RIS, and is read through a clone-out API
//! ([`AltArchive::read`]) so the caller formats the reply after dropping the
//! terminal lock. [`Terminal::set_alt_archive_enabled`] and
//! [`Terminal::set_alt_archive_budget`] set it per session.
//!
//! The retained-row budget is a process total: sessions that joined
//! [`AltArchiveBudget::process`] each retain at most `4 MiB / live sessions`,
//! floored at [`ALT_ARCHIVE_MIN_SHARE`]. A session lowers only its own retention,
//! on its next commit, so no lock crosses sessions; dropping the archive gives the
//! share back.
//!
//! A self-update handoff carries the archive to the adopting process
//! ([`AltArchiveCarry`]): origin, indices, the differ's state and a tail of rows
//! the host chose, never shorter than the rows the differ may still point back at.
//! [`Terminal::alt_archive_carry_head`] takes the counters (and, time allowing, the
//! differ state) under the checkpoint's lock; [`Terminal::alt_archive_carry_rows`]
//! and [`Terminal::alt_archive_carry_differ`] attach the rest afterwards while the
//! [`AltArchiveFence`] holds. [`Terminal::alt_archive_import`] installs it and never
//! panics on its input; an archive that is off refuses it.

use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use aterm_hash::{FxHashMap, FxHasher};

use super::Terminal;
use crate::grid::Grid;

/// Default retained-row accounting budget for one archive on its own: 4 MiB,
/// charged per row by [`alt_archive_row_charge`]. Not a bound on total
/// allocation or RSS. An archive in a budget pool ([`AltArchive::share_budget`])
/// is held to the smaller of this and its share of the pool.
pub const ALT_ARCHIVE_DEFAULT_BUDGET: usize = 4 * 1024 * 1024;
/// Retained-row budget the live sessions of one process share: 4 MiB in total,
/// not 4 MiB each ([`AltArchiveBudget::process`]).
pub const ALT_ARCHIVE_TOTAL_BUDGET: usize = ALT_ARCHIVE_DEFAULT_BUDGET;
/// Floor under one sharing session's budget: 1/16th of the total (256 KiB).
/// Past 16 live sessions the process total grows again, so an extra tab never
/// starves the others' archives to nothing.
pub const ALT_ARCHIVE_MIN_SHARE: usize = ALT_ARCHIVE_TOTAL_BUDGET / 16;
/// Hard cap on archived rows regardless of the byte budget.
pub const ALT_ARCHIVE_MAX_ROWS: usize = 65_536;
/// Fixed charge per retained row: one deque element ([`ArchivedRow`]) plus the
/// two reference counts of its `Arc<str>`. Allocator overhead and spare deque
/// slots are not charged.
pub const ALT_ARCHIVE_ROW_OVERHEAD: usize = size_of::<ArchivedRow>() + 2 * size_of::<usize>();

/// Retained-row charge for `len` text bytes: the fixed row overhead plus text
/// rounded up to 16 bytes (an accounting policy, not an allocator model).
/// Shared by row admission, carry selection and callers.
#[must_use]
pub const fn alt_archive_row_charge(len: usize) -> usize {
    len.next_multiple_of(16) + ALT_ARCHIVE_ROW_OVERHEAD
}
/// Minimum visible chars for a row to vote (after trimming both ends).
const MIN_ANCHOR_CHARS: usize = 4;
/// Votes a shift needs (fewer when the previous frame has fewer anchors).
const NEED_VOTES: usize = 3;
/// How many screens of archived rows a reanchor searches.
const REANCHOR_SCREENS: usize = 8;
/// Minimum spacing of epilogue (no-2026) commits, in `process_now` time.
const FALLBACK_COMMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// Why the archive is not contiguous after a given row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AltArchiveGapKind {
    /// A redraw with no reliable overlap: the old screen's rows were flushed and
    /// the new screen does not continue them. Also a scroll-back past the
    /// oldest row the archive holds: the rows it re-shows cannot be matched,
    /// and are archived again, after this gap, as they scroll off.
    Jump,
    /// The screen changed size; the old screen was flushed.
    Resize,
    /// The app left the alternate screen; its last screen was flushed.
    Leave,
    /// A full reset (RIS / host reset) wiped the archive.
    Reset,
    /// A checkpoint restore replaced the grids wholesale.
    Restore,
}

impl AltArchiveGapKind {
    /// Lowercase wire name (`jump`, `resize`, `leave`, `reset`, `restore`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jump => "jump",
            Self::Resize => "resize",
            Self::Leave => "leave",
            Self::Reset => "reset",
            Self::Restore => "restore",
        }
    }

    /// Inverse of [`as_str`](Self::as_str); `None` for any other word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "jump" => Self::Jump,
            "resize" => Self::Resize,
            "leave" => Self::Leave,
            "reset" => Self::Reset,
            "restore" => Self::Restore,
            _ => return None,
        })
    }
}

/// A discontinuity: archived row `after` and row `after + 1` are not adjacent
/// on any screen. `after == 0` never occurs (a gap before the first row says
/// nothing and is not recorded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AltArchiveGap {
    /// Index of the last archived row before the discontinuity.
    pub after: u64,
    /// What caused it.
    pub kind: AltArchiveGapKind,
}

/// What to clone out of the archive: rows with index strictly greater than
/// `since`, at most `max_rows` of them, oldest-first or newest-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltArchiveQuery {
    /// Exclusive lower bound (0 = from the beginning). Clamped to `last`.
    pub since: u64,
    /// Page size.
    pub max_rows: usize,
    /// `false`: the oldest `max_rows` rows after `since` (paging forward with
    /// `since=<last>`); `true`: the newest `max_rows` rows (a tail).
    pub newest: bool,
}

impl AltArchiveQuery {
    /// Oldest-first page of at most `max_rows` rows after `since`.
    #[must_use]
    pub const fn oldest(since: u64, max_rows: usize) -> Self {
        Self {
            since,
            max_rows,
            newest: false,
        }
    }

    /// The newest `max_rows` rows after `since`.
    #[must_use]
    pub const fn newest(since: u64, max_rows: usize) -> Self {
        Self {
            since,
            max_rows,
            newest: true,
        }
    }
}

/// A clone-out of the archive, taken under the terminal lock and formatted
/// after it is released. Row `m` of `rows` has index `first + m`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltArchiveRead {
    /// The rows, oldest first, as `text` showed them (control chars already
    /// collapsed to spaces, trailing blanks trimmed).
    pub rows: Vec<Arc<str>>,
    /// Index of `rows[0]` (`last + 1` when nothing was returned).
    pub first: u64,
    /// Newest archived index; 0 when nothing was ever archived. The next page
    /// of an oldest-first read starts after [`page_last`](Self::page_last),
    /// not after this.
    pub last: u64,
    /// Oldest index still retained (`last + 1` when the archive is empty).
    pub oldest: u64,
    /// Rows after `since` that were evicted (or wiped) before this read.
    pub lost: u64,
    /// Every row ever evicted or wiped from this archive.
    pub lost_total: u64,
    /// Gaps with `after >= since`, oldest first.
    pub gaps: Vec<AltArchiveGap>,
    /// How many rows at the top of the screen (below any pinned header) are
    /// already archived — a reader joining archive + screen skips that many.
    pub back: usize,
    /// Index of the archived row shown at the top of the scrolling region when
    /// `back > 0` (then screen row `pin + m` is archived row `back_at + m` for
    /// `m < back`); 0 otherwise. Usually `last - back + 1`.
    pub back_at: u64,
    /// Screen rows pinned above the scrolling region in the last shift.
    pub pin: usize,
    /// Baseline generation: bumped whenever the screen/archive relationship is
    /// rebuilt (alt enter/leave, resize, reset, restore).
    pub epoch: u32,
    /// Host-assigned identity of the process that started this archive (see
    /// [`AltArchive::set_origin`]; a handoff that carries the archive keeps
    /// it); indices are only comparable within one origin.
    pub origin: u64,
    /// More rows matched than `max_rows` allowed.
    pub more: bool,
    /// Whether the archive is recording.
    pub enabled: bool,
}

impl AltArchiveRead {
    /// Index of the last row this read returned (`last` when it returned
    /// none): the exclusive `since` of the next page or poll.
    #[must_use]
    pub fn page_last(&self) -> u64 {
        match self.rows.len() {
            0 => self.last,
            n => self.first + n as u64 - 1,
        }
    }
}

/// Where an archive stood when a self-update handoff captured it, inside the
/// freeze: the rows exported afterwards, off the frozen thread, are the rows
/// the captured screen continues only while the archive still stands here
/// ([`AltArchive::carry_rows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltArchiveFence {
    /// The archive's origin.
    pub origin: u64,
    /// Its baseline generation.
    pub epoch: u32,
    /// The oldest retained index.
    pub first: u64,
    /// The newest archived index.
    pub last: u64,
    /// Rows ever evicted or wiped.
    pub lost: u64,
    /// Retained gaps.
    pub gaps: usize,
    /// A fingerprint of the differ's state (the last frame's rows and width,
    /// the chrome, the pin, the re-shown run, the rows below): a differ state
    /// taken off the frozen thread is the one the freeze saw only while it is
    /// unchanged ([`AltArchive::carry_differ`]). The rows do not depend on
    /// it ([`AltArchive::carry_rows`] ignores it).
    pub differ: u64,
}

impl AltArchiveFence {
    /// The fence without the differ's fingerprint: what the retained rows
    /// depend on.
    const fn rows_part(self) -> Self {
        Self { differ: 0, ..self }
    }
}

/// The differ's state against the screen a handoff captured: what the next
/// frame is compared with. Screen-sized (the last committed frame, and the
/// rows it pushed out at the bottom), so it is taken inside the handoff
/// freeze beside the checkpoint of that same screen — or, when the freeze had
/// no time to spare, after it while the fence's fingerprint of it holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AltArchiveDiffer {
    /// The last committed frame: its width and its rows' text, as `text`
    /// showed them. `None`: no frame was committed on this baseline.
    pub prev: Option<(u16, Vec<String>)>,
    /// Fixed bottom chrome height (`None` until a frame was diffed).
    pub chrome: Option<usize>,
    /// Rows pinned above the scrolling region.
    pub pin: usize,
    /// Archived rows the screen shows again, from screen row `pin`.
    pub debt: usize,
    /// Index of the archived row at screen row `pin` while `debt > 0`.
    pub debt_at: u64,
    /// Rows that left the bottom of the scrolling region, top first.
    pub below: Vec<String>,
    /// The app paces its frames with DEC 2026 closes (the epilogue commit
    /// stays out of its way).
    pub esu_seen: bool,
}

/// One session's archive as a self-update handoff carries it to the process
/// that adopts the session: the counters, the gaps, the tail of the rows (a
/// carry may leave older rows out: they count in `lost`), and the differ's
/// state. Row hashes and anchor votes are never carried — [`AltArchive::import`]
/// recomputes them from the text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AltArchiveCarry {
    /// The archive's origin: kept, so `<origin>:<index>` marks minted before
    /// the handoff still name rows of this archive.
    pub origin: u64,
    /// Index of `rows[0]` (`last + 1` when no row is carried).
    pub first: u64,
    /// Rows evicted, wiped, or left out of this carry.
    pub lost: u64,
    /// Lowest index a reanchor or a scroll-back may point at.
    pub floor: u64,
    /// Baseline generation.
    pub epoch: u32,
    /// Whether the archive was recording.
    pub enabled: bool,
    /// Gaps a reader of the carried rows needs, oldest first.
    pub gaps: Vec<AltArchiveGap>,
    /// The carried rows, oldest first.
    pub rows: Vec<Arc<str>>,
    /// The differ's state; `None` when it was not carried (the adopting side
    /// then starts a new baseline on the screen it restored, after a gap).
    pub differ: Option<AltArchiveDiffer>,
}

impl AltArchiveCarry {
    /// The newest index this carry names (`first - 1` plus its rows).
    #[must_use]
    pub fn last(&self) -> u64 {
        (self.first + self.rows.len() as u64).saturating_sub(1)
    }
}

/// What [`AltArchive::import`] did with a carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltArchiveImport {
    /// Counters, gaps, rows and the differ's state were installed: the next
    /// frame is diffed exactly as the exporting process would have diffed it.
    Exact,
    /// Counters, gaps and rows were installed; the differ's state was not
    /// carried or did not describe a frame, so the baseline was dropped.
    NoBaseline,
    /// Nothing was installed: this archive is off, or the carry's indices are
    /// out of any range an archive reaches.
    Refused,
}

/// Above this an index is not one an archive can reach (one row per commit,
/// 2^62 of them): an imported index past it would overflow later arithmetic.
const MAX_CARRIED_INDEX: u64 = 1 << 62;

#[derive(Debug, Clone)]
struct ArchivedRow {
    text: Arc<str>,
    hash: u64,
    anchor: bool,
}

impl ArchivedRow {
    /// This row's charge against the retained-row budget, using the same
    /// accounting policy as candidate admission and handoff carry selection.
    fn cost(&self) -> usize {
        alt_archive_row_charge(self.text.len())
    }
}

/// One committed frame: trimmed row text with its hash and anchor eligibility.
/// The `String`s are reused across commits (the two frames swap roles).
#[derive(Debug, Default)]
struct Frame {
    text: Vec<String>,
    hash: Vec<u64>,
    anchor: Vec<bool>,
    /// Non-blank and not only box/rule art: a row that says something, however
    /// short (the eligibility of the weak vote and of the sparse test).
    content: Vec<bool>,
    cols: u16,
}

impl Frame {
    fn rows(&self) -> usize {
        self.text.len()
    }

    /// The row hashes: all an unchanged frame (Rule 2) needs.
    fn derive(&mut self) {
        self.hash.clear();
        self.hash.extend(self.text.iter().map(|s| row_hash(s)));
    }

    /// The anchor and content eligibility the differ votes with, after
    /// [`Self::derive`].
    fn derive_votes(&mut self) {
        self.anchor.clear();
        self.content.clear();
        self.anchor
            .extend(self.text.iter().map(|s| is_anchor_text(s)));
        self.content
            .extend(self.text.iter().map(|s| is_content_text(s)));
    }

    fn blank(&self, i: usize) -> bool {
        self.text[i].is_empty()
    }

    /// Whether every row is blank (a cleared screen).
    fn all_blank(&self) -> bool {
        self.text.iter().all(String::is_empty)
    }
}

/// Reused per-commit scratch (maps keep their capacity across frames).
#[derive(Debug, Default)]
struct Scratch {
    prev_idx: FxHashMap<u64, (u32, usize)>,
    cur_idx: FxHashMap<u64, (u32, usize)>,
    /// Indexed by `d + T - 1`: (votes, cur positions of the topmost and the
    /// lowest voter).
    votes: Vec<(u32, usize, usize)>,
    tail_idx: FxHashMap<u64, usize>,
    j_votes: FxHashMap<usize, u32>,
    picks: Vec<usize>,
}

/// A retained-row budget that live archives share: one total divided by the
/// number of archives currently drawing on it, each held to `total / live`
/// (never below its `min_share` floor).
///
/// Shares are equal, not first-come-first-served, so each session's limit is a
/// pure function of the live count and eviction stays oldest-first and
/// deterministic within each archive.
///
/// An archive reads its share on every committed frame and whenever its budget
/// is set; it lowers only its own retention, so no lock crosses sessions. An
/// idle session keeps what it holds until it draws again. The total therefore
/// converges rather than holding at every instant: if sessions fill in turn and
/// go idle, session `k` holds `total / k` and the sum is `total * H_n`; one frame
/// per session brings it back inside `total`.
/// Only archives holding rows count as live, so tabs not running a fullscreen
/// app do not shrink the share of the one that is.
#[derive(Debug)]
pub struct AltArchiveBudget {
    total: usize,
    min_share: usize,
    live: AtomicUsize,
}

impl AltArchiveBudget {
    /// A pool of `total` bytes, no share smaller than `min_share` (clamped to
    /// `total`: a floor above the total would hand out more than there is).
    #[must_use]
    pub fn new(total: usize, min_share: usize) -> Arc<Self> {
        Arc::new(Self {
            total,
            min_share: min_share.min(total),
            live: AtomicUsize::new(0),
        })
    }

    /// The process-wide pool: [`ALT_ARCHIVE_TOTAL_BUDGET`] shared by every live
    /// session whose host joined it ([`Terminal::set_alt_archive_shared`]). A
    /// session on its own keeps the whole [`ALT_ARCHIVE_DEFAULT_BUDGET`].
    #[must_use]
    pub fn process() -> Arc<Self> {
        static POOL: OnceLock<Arc<AltArchiveBudget>> = OnceLock::new();
        Arc::clone(
            POOL.get_or_init(|| {
                AltArchiveBudget::new(ALT_ARCHIVE_TOTAL_BUDGET, ALT_ARCHIVE_MIN_SHARE)
            }),
        )
    }

    /// Bytes this pool divides between its archives.
    #[must_use]
    #[cfg(test)]
    pub fn total(&self) -> usize {
        self.total
    }

    /// Archives currently drawing on this pool: joined, enabled and holding rows.
    /// An empty archive costs nothing and is not counted, so idle tabs do not
    /// shrink the share of the one running a fullscreen app.
    #[must_use]
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// What one archive may retain right now: `total / live`, floored at
    /// `min_share`. With nothing live yet (the archive asking is about
    /// to be the first), the whole total.
    #[must_use]
    pub fn share(&self) -> usize {
        (self.total / self.live().max(1)).max(self.min_share)
    }
}

/// One archive's membership of a pool. `counted` tracks whether it is in the
/// live count so increment and decrement stay paired, including on drop (a
/// closed tab gives its share back).
#[derive(Debug)]
struct BudgetShare {
    pool: Arc<AltArchiveBudget>,
    counted: bool,
}

impl BudgetShare {
    fn new(pool: Arc<AltArchiveBudget>) -> Self {
        Self {
            pool,
            counted: false,
        }
    }

    fn set_counted(&mut self, counted: bool) {
        if counted == self.counted {
            return;
        }
        if counted {
            self.pool.live.fetch_add(1, Ordering::Relaxed);
        } else {
            self.pool.live.fetch_sub(1, Ordering::Relaxed);
        }
        self.counted = counted;
    }
}

impl Drop for BudgetShare {
    fn drop(&mut self) {
        self.set_counted(false);
    }
}

/// The pure differ + store. See the module docs.
pub struct AltArchive {
    rows: VecDeque<ArchivedRow>,
    bytes: usize,
    budget: usize,
    /// The shared budget this archive draws on, if a host joined it to one:
    /// its retention is then the smaller of `budget` and the pool's share.
    share: Option<BudgetShare>,
    max_rows: usize,
    /// Index of `rows[0]`; indices start at 1.
    first: u64,
    gaps: VecDeque<AltArchiveGap>,
    lost: u64,
    epoch: u32,
    origin: u64,
    enabled: bool,
    prev: Frame,
    cur: Frame,
    have_prev: bool,
    /// Content key (grid content generation) `prev` was committed at; only
    /// meaningful while `have_prev` (every lineage change drops `prev`).
    prev_key: Option<u64>,
    /// Rows at the top of the scrolling region (screen rows `[pin, pin+debt)`)
    /// that are already archived, as archived rows `[debt_at, debt_at+debt)`.
    /// Usually the archive's last rows (the app scrolled back); after a reanchor
    /// they can sit mid-archive (a view toggled away and back).
    debt: usize,
    /// Absolute index of the archived row shown at screen row `pin` (meaningful
    /// while `debt > 0`).
    debt_at: u64,
    /// Rows pinned above the scrolling region in the last shift.
    pin: usize,
    /// Fixed bottom chrome height: measured on a shift, then only narrowed by
    /// later diffs on this baseline. `None` until a frame was diffed: a leave
    /// or restore flush then takes the whole screen, and a resize leaves out
    /// rows the new frame still shows at the bottom
    /// ([`resize_region`](Self::resize_region)).
    chrome: Option<usize>,
    /// Rows that left the bottom of the scrolling region when the content moved
    /// down, top first: displayed, not archived. They re-enter when the app
    /// scrolls forward; a flush (jump, resize, leave, restore) archives the rest.
    below: VecDeque<ArchivedRow>,
    /// Lowest index a reanchor or a scroll-back may point at: rows archived
    /// before the app's current alt-screen run (a previous app, across a
    /// leave/reset/restore gap) are never the rows this screen shows again.
    floor: u64,
    scratch: Scratch,
}

impl std::fmt::Debug for AltArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AltArchive")
            .field("rows", &self.rows.len())
            .field("bytes", &self.bytes)
            .field("first", &self.first)
            .field("last", &self.last())
            .field("gaps", &self.gaps.len())
            .field("lost", &self.lost)
            .field("epoch", &self.epoch)
            .field("debt", &self.debt)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl Default for AltArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl AltArchive {
    /// An enabled archive with the default budget.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: VecDeque::new(),
            bytes: 0,
            budget: ALT_ARCHIVE_DEFAULT_BUDGET,
            share: None,
            max_rows: ALT_ARCHIVE_MAX_ROWS,
            first: 1,
            gaps: VecDeque::new(),
            lost: 0,
            epoch: 0,
            origin: 0,
            enabled: true,
            prev: Frame::default(),
            cur: Frame::default(),
            have_prev: false,
            prev_key: None,
            debt: 0,
            debt_at: 0,
            pin: 0,
            chrome: None,
            below: VecDeque::new(),
            floor: 1,
            scratch: Scratch::default(),
        }
    }

    /// An archive with an explicit byte budget and row cap (tests, embedders).
    #[must_use]
    #[cfg(test)]
    pub fn with_limits(budget: usize, max_rows: usize) -> Self {
        let mut a = Self::new();
        a.budget = budget;
        a.max_rows = max_rows.max(1);
        a.enabled = budget > 0;
        a
    }

    // ----------------------------------------------------------------- reads

    /// Newest archived index (0 when nothing was ever archived).
    #[must_use]
    pub fn last(&self) -> u64 {
        self.first + self.rows.len() as u64 - 1
    }

    /// Oldest retained index (`last + 1` when empty).
    #[must_use]
    pub fn oldest(&self) -> u64 {
        self.first
    }

    /// Retained row count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether no rows are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Bytes charged against the budget.
    #[must_use]
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// This archive's own byte budget (0 = off) — what a host set for it,
    /// before any shared pool. [`Self::effective_budget`] is what it may
    /// actually retain.
    #[must_use]
    #[cfg(test)]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// What this archive may retain right now: its own budget, and no more
    /// than its share of the pool it joined ([`Self::share_budget`]). Eviction
    /// is measured against this, so it moves as sessions open and close.
    #[must_use]
    pub fn effective_budget(&self) -> usize {
        match &self.share {
            Some(s) => self.budget.min(s.pool.share()),
            None => self.budget,
        }
    }

    /// The shared budget this archive draws on, if any.
    #[must_use]
    #[cfg(test)]
    pub fn shared_budget(&self) -> Option<&Arc<AltArchiveBudget>> {
        self.share.as_ref().map(|s| &s.pool)
    }

    /// Rows ever evicted or wiped.
    #[must_use]
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// Baseline generation (see [`AltArchiveRead::epoch`]).
    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Host-assigned origin (0 until set).
    #[must_use]
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// Archived rows re-shown at the top of the screen (the debt).
    #[must_use]
    pub fn back(&self) -> usize {
        self.debt
    }

    /// Index of the archived row shown at the top of the scrolling region when
    /// [`back`](Self::back) is nonzero (0 otherwise).
    #[must_use]
    pub fn back_at(&self) -> u64 {
        if self.debt > 0 { self.debt_at } else { 0 }
    }

    /// Whether the archive is recording.
    #[must_use]
    #[cfg(test)]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The retained gaps, oldest first.
    pub fn gaps(&self) -> impl Iterator<Item = AltArchiveGap> + '_ {
        self.gaps.iter().copied()
    }

    /// The text of the retained row with index `index`, if any.
    #[must_use]
    pub fn row(&self, index: u64) -> Option<&str> {
        let off = usize::try_from(index.checked_sub(self.first)?).ok()?;
        self.rows.get(off).map(|r| &*r.text)
    }

    /// Every retained row's text, oldest first (tests and diagnostics; the
    /// control plane uses [`read`](Self::read)).
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.text.to_string()).collect()
    }

    /// Clone rows out per `q`. Cheap under a lock: one `Arc` clone per row, no
    /// formatting. `q.since` above `last` is clamped to `last`.
    #[must_use]
    pub fn read(&self, q: AltArchiveQuery) -> AltArchiveRead {
        let last = self.last();
        let since = q.since.min(last);
        let lo = (since + 1).max(self.first);
        let avail = usize::try_from((last + 1).saturating_sub(lo)).unwrap_or(usize::MAX);
        let take = avail.min(q.max_rows);
        let start = if q.newest { last + 1 - take as u64 } else { lo };
        let off = usize::try_from(start - self.first).unwrap_or(0);
        let rows = self
            .rows
            .iter()
            .skip(off)
            .take(take)
            .map(|r| Arc::clone(&r.text))
            .collect();
        AltArchiveRead {
            rows,
            first: start,
            last,
            oldest: self.first,
            lost: self.first.saturating_sub(since + 1),
            lost_total: self.lost,
            gaps: self
                .gaps
                .iter()
                .filter(|g| g.after >= since)
                .copied()
                .collect(),
            back: self.debt,
            back_at: self.back_at(),
            pin: self.pin,
            epoch: self.epoch,
            origin: self.origin,
            more: take < avail,
            enabled: self.enabled,
        }
    }

    // --------------------------------------------------------------- control

    /// Set the host-assigned origin: unique per process that started an
    /// archive (e.g. `pid << 32 ^ launch nanos`) so indices from another
    /// archive are never mistaken for this one's. A handoff that carries the
    /// archive keeps its origin ([`AltArchive::import`]): the indices go on.
    pub fn set_origin(&mut self, origin: u64) {
        self.origin = origin;
    }

    /// Set the byte budget. 0 turns the archive off and wipes it (rows count as
    /// lost); a smaller budget evicts down to it. An archive sharing a pool is
    /// still held to its share when that is smaller
    /// ([`Self::effective_budget`]).
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        // `wipe` gives the share back; `evict` counts this archive in.
        if budget == 0 {
            self.wipe();
            self.enabled = false;
        } else {
            self.enabled = true;
            self.evict();
        }
    }

    /// Draw on `pool` (or, with `None`, only on this archive's own budget),
    /// evicting down to the resulting share. Joining takes no share until the
    /// archive holds rows ([`AltArchiveBudget::live`]); dropping the archive
    /// gives its share back.
    pub fn share_budget(&mut self, pool: Option<Arc<AltArchiveBudget>>) {
        let same = match (&self.share, &pool) {
            (Some(s), Some(p)) => Arc::ptr_eq(&s.pool, p),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        // Leave the old pool before joining the new one, so an archive moving
        // between pools is never counted twice.
        self.share = None;
        self.share = pool.map(BudgetShare::new);
        self.sync_share();
        self.evict();
    }

    /// Count this archive among its pool's live archives exactly while it is
    /// enabled and holds at least one row (see [`AltArchiveBudget::live`]).
    /// Called wherever `bytes` can cross zero: both ends of [`Self::evict`] and
    /// [`Self::wipe`].
    fn sync_share(&mut self) {
        let drawing = self.enabled && self.bytes > 0;
        if let Some(share) = &mut self.share {
            share.set_counted(drawing);
        }
    }

    /// Turn recording on or off. Off wipes (rows count as lost); on starts from
    /// a fresh baseline.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        if enabled {
            if self.budget == 0 {
                self.budget = ALT_ARCHIVE_DEFAULT_BUDGET;
            }
            self.enabled = true;
            self.drop_prev();
        } else {
            self.wipe();
            self.enabled = false;
        }
        self.sync_share();
    }

    // -------------------------------------------------------------- commits

    /// Commit one frame given as row strings (the pure entry point: tests and
    /// embedders). Each row is normalized exactly like extracted grid text.
    #[cfg(test)]
    pub fn commit_rows<S: AsRef<str>>(&mut self, rows: &[S], cols: u16) {
        if !self.enabled {
            return;
        }
        let bufs = self.frame_buffers(rows.len());
        for (buf, s) in bufs.iter_mut().zip(rows) {
            buf.clear();
            buf.push_str(s.as_ref());
            normalize_row(buf);
        }
        self.commit_prepared(cols, None);
    }

    /// Whether a frame keyed `key` at these dims is the one already committed.
    fn is_unchanged(&self, key: u64, rows: usize, cols: u16) -> bool {
        self.have_prev
            && self.prev_key == Some(key)
            && self.prev.rows() == rows
            && self.prev.cols == cols
    }

    /// The next frame's row buffers (`rows` of them, capacity reused), to be
    /// filled with normalized row text and then committed.
    fn frame_buffers(&mut self, rows: usize) -> &mut [String] {
        self.cur.text.resize_with(rows, String::new);
        &mut self.cur.text
    }

    /// Fill the next frame's row buffers with `grid`'s screen rows, normalized
    /// exactly like `text` shows them.
    fn fill_from_grid(&mut self, grid: &Grid) {
        let rows = grid.rows();
        let bufs = self.frame_buffers(usize::from(rows));
        for (r, buf) in (0..rows).zip(bufs.iter_mut()) {
            grid.row_text_screen_into(r, buf);
            normalize_row(buf);
        }
    }

    /// Commit `grid`'s screen rows (the active alt grid) as the next frame,
    /// keyed by its content generation so a redundant ESU does no work. Shared
    /// by the `Terminal` hook and the handler's orphaned-screen leave.
    fn commit_grid(&mut self, grid: &Grid) {
        let rows = grid.rows();
        let cols = grid.cols();
        if rows == 0 || cols == 0 {
            return;
        }
        let key = grid.content_gen();
        if self.is_unchanged(key, usize::from(rows), cols) {
            return;
        }
        self.fill_from_grid(grid);
        self.commit_prepared(cols, Some(key));
    }

    /// Commit the frame in `cur` (filled through [`frame_buffers`]).
    fn commit_prepared(&mut self, cols: u16, key: Option<u64>) {
        // A pool share falls when another session opens; coming within it here
        // (one relaxed load when nothing is to evict) makes every drawing
        // session converge on its next commit.
        self.evict();
        let rows = self.cur.rows();
        if rows == 0 {
            return;
        }
        // A cleared screen is not a frame: diffed it reads as a jump that would
        // archive the chrome, installed it would be a false baseline. The frame
        // before it stays the baseline.
        if self.cur.all_blank() {
            return;
        }
        self.cur.cols = cols;
        self.cur.derive();
        // Rule 2: identical text on the same screen (a redundant ESU, a
        // cursor-only or SGR-only frame). It reads the hashes alone, so it runs
        // before the votes are derived.
        if self.have_prev
            && self.prev.cols == cols
            && self.prev.rows() == rows
            && self.prev.hash == self.cur.hash
        {
            self.prev_key = key;
            return;
        }
        self.cur.derive_votes();
        // Rule 1: no baseline, or the screen changed size.
        if !self.have_prev {
            self.install_cur(key);
            return;
        }
        if self.prev.cols != cols || self.prev.rows() != rows {
            let t_prev = self.resize_region();
            self.flush_prev_rows(0, t_prev);
            self.push_gap(AltArchiveGapKind::Resize);
            self.epoch = self.epoch.wrapping_add(1);
            self.drop_prev();
            self.install_cur(key);
            return;
        }
        // Rule 3: fixed chrome at the bottom (composer, footer).
        let mut b = 0;
        while b < rows && self.prev.hash[rows - 1 - b] == self.cur.hash[rows - 1 - b] {
            b += 1;
        }
        let t = rows - b;
        self.diff(t, b);
        self.check_debt(t);
        self.install_cur(key);
    }

    fn install_cur(&mut self, key: Option<u64>) {
        std::mem::swap(&mut self.prev, &mut self.cur);
        self.have_prev = true;
        self.prev_key = key;
    }

    fn drop_prev(&mut self) {
        self.have_prev = false;
        self.prev_key = None;
        self.debt = 0;
        self.pin = 0;
        self.chrome = None;
        self.below.clear();
    }

    /// The scrolling region of the last committed frame: everything above the
    /// chrome (the whole screen while no frame was diffed on this baseline).
    fn prev_region(&self) -> usize {
        let r = self.prev.rows();
        r - self.chrome.unwrap_or(0).min(r)
    }

    /// What a resize flushes of the last committed frame (Rule 1): its scrolling
    /// region once a diff measured the chrome. Until then the chrome is unknown,
    /// so the run of rows the new frame still shows at the bottom (aligned at the
    /// bottom, compared by [`Self::same_across_resize`]) is left out. Over-counting
    /// is safe: real content still on screen is archived when it scrolls off.
    fn resize_region(&self) -> usize {
        if self.chrome.is_some() {
            return self.prev_region();
        }
        let (p, c) = (self.prev.rows(), self.cur.rows());
        let mut b = 0;
        while b < p.min(c) && self.same_across_resize(p - 1 - b, c - 1 - b) {
            b += 1;
        }
        p - b
    }

    /// Whether `prev` row `i` and `cur` row `j` show the same thing at two sizes:
    /// equal hashes, or equal text once box/rule art and spaces are removed (a
    /// `────` rule, or a `│ > text │` composer box, redrawn at a new width).
    fn same_across_resize(&self, i: usize, j: usize) -> bool {
        self.prev.hash[i] == self.cur.hash[j]
            || said_text(&self.prev.text[i]).eq(said_text(&self.cur.text[j]))
    }

    /// A frame that did not shift: the chrome is at most what this one kept.
    fn narrow_chrome(&mut self, b: usize) {
        self.chrome = Some(self.chrome.map_or(b, |c| c.min(b)));
    }

    /// Votes a shift over `[0, t)` needs: [`NEED_VOTES`], or fewer when the
    /// previous frame has fewer rows eligible to vote (at least 1).
    fn need(&self, t: usize, weak: bool) -> usize {
        let eligible = if weak {
            &self.prev.content
        } else {
            &self.prev.anchor
        };
        eligible[..t]
            .iter()
            .filter(|&&a| a)
            .count()
            .clamp(1, NEED_VOTES)
    }

    /// Rules 4-8 over the region `[0, t)`; `b` is the fixed chrome height.
    fn diff(&mut self, t: usize, b: usize) {
        let rows = self.cur.rows();
        let (best, moved) = self.vote(t, false);
        let mut need = self.need(t, false);
        let (mut vote, mut region) = (best, t);
        // Rows that did not move below a block that did (a footer over a moving
        // block, so rule 3 found no chrome) outvote the few rows a big scroll
        // leaves overlapping: they are this frame's chrome.
        if best.d == 0
            && best.votes >= need
            && let Some(floor) = self.fixed_floor(t, moved)
            && moved.votes >= self.need(floor, false)
        {
            need = self.need(floor, false);
            (vote, region) = (moved, floor);
        }
        // No reliable anchor vote — a screen with too few distinctive rows
        // (line numbers, a word list, `seq` in less), or a real jump: let every
        // row that says something vote, uniqueness still required. Only a move
        // is taken from it: short rows that stayed put prove no stillness (a
        // jump can leave a `fi` where it was), so rule 8 still judges that.
        if vote.votes < need {
            let (weak, _) = self.vote(t, true);
            let weak_need = self.need(t, true);
            if weak.d != 0 && weak.votes >= weak_need {
                (vote, need) = (weak, weak_need);
            }
        }
        if vote.votes >= need {
            match vote.d.cmp(&0) {
                // Rule 5: anchors did not move — in-place edits only.
                std::cmp::Ordering::Equal => self.narrow_chrome(b),
                // Rule 6: content moved up by k.
                std::cmp::Ordering::Greater => {
                    self.chrome = Some(rows - region);
                    self.shift_up(vote.d.unsigned_abs(), vote.top);
                }
                // Rule 7: content moved down (a block above shrank, or the user
                // scrolled the app back).
                std::cmp::Ordering::Less => {
                    self.chrome = Some(rows - region);
                    self.shift_down(vote, region);
                }
            }
            self.settle_below();
            return;
        }
        self.narrow_chrome(b);
        // Rule 8: no reliable vote.
        let changed = (0..t)
            .filter(|&i| self.prev.hash[i] != self.cur.hash[i])
            .count();
        if changed <= (t / 4).max(2) {
            self.settle_below();
            return; // an in-place edit (spinner, a tool line finishing)
        }
        // Sparse: a near-empty screen (or one of box art only) says too little
        // to call a jump. A screen full of short rows is not sparse: the rows it
        // displayed are flushed like any other.
        let says = |f: &Frame| f.content[..t].iter().filter(|&&c| c).count();
        if says(&self.prev) < NEED_VOTES && says(&self.cur) < NEED_VOTES {
            self.settle_below();
            return;
        }
        self.jump(t);
    }

    /// Anchor voting (rule 4) over `[0, t)`: rows that are eligible (anchors, or
    /// with `weak` any row with content) and unique in both frames vote for the
    /// offset `d = pos_prev - pos_cur` that maps one onto the other. Returns the
    /// winner and the best nonzero offset.
    fn vote(&mut self, t: usize, weak: bool) -> (Vote, Vote) {
        let s = &mut self.scratch;
        let (prev_ok, cur_ok) = if weak {
            (&self.prev.content, &self.cur.content)
        } else {
            (&self.prev.anchor, &self.cur.anchor)
        };
        index_anchors(&self.prev.hash, prev_ok, t, &mut s.prev_idx);
        index_anchors(&self.cur.hash, cur_ok, t, &mut s.cur_idx);
        s.votes.clear();
        s.votes.resize(2 * t - 1, (0, 0, 0));
        for (i, &ok) in cur_ok[..t].iter().enumerate() {
            if !ok {
                continue;
            }
            let h = self.cur.hash[i];
            if s.cur_idx.get(&h).is_none_or(|e| e.0 != 1) {
                continue;
            }
            let Some(&(1, p)) = s.prev_idx.get(&h) else {
                continue;
            };
            if self.prev.text[p] != self.cur.text[i] {
                continue; // a hash collision never votes
            }
            let slot = &mut s.votes[p + t - 1 - i];
            if slot.0 == 0 {
                slot.1 = i;
            }
            slot.0 += 1;
            slot.2 = i;
        }
        let mut best = Vote::default();
        let mut moved = Vote::default();
        for (k, &(v, top, bot)) in s.votes.iter().enumerate() {
            if v == 0 {
                continue;
            }
            // Rows are bounded by u16, so neither cast can wrap.
            let cand = Vote {
                d: k.cast_signed() - (t - 1).cast_signed(),
                votes: v as usize,
                top,
                bot,
            };
            if cand.beats(best) {
                best = cand;
            }
            if cand.d != 0 && cand.beats(moved) {
                moved = cand;
            }
        }
        (best, moved)
    }

    /// Whether cur row `row` voted for `offset` in the last (anchor) vote.
    fn votes_for(&self, row: usize, offset: isize) -> bool {
        if !self.cur.anchor[row] {
            return false;
        }
        let hash = self.cur.hash[row];
        let scratch = &self.scratch;
        if scratch.cur_idx.get(&hash).is_none_or(|e| e.0 != 1) {
            return false;
        }
        let Some(&(1, pos)) = scratch.prev_idx.get(&hash) else {
            return false;
        };
        pos.cast_signed() - row.cast_signed() == offset && self.prev.text[pos] == self.cur.text[row]
    }

    /// Where fixed rows start under a block that moved: the first anchor below
    /// `moved`'s voters that stayed put, when none sits among them (rows that
    /// stayed put above the block are a pinned header, which a shift handles).
    fn fixed_floor(&self, t: usize, moved: Vote) -> Option<usize> {
        if moved.votes == 0 {
            return None;
        }
        (moved.top..t)
            .find(|&i| self.votes_for(i, 0))
            .filter(|&i| i > moved.bot)
    }

    /// Leading rows equal at the same position, stopping before `limit`.
    fn leading_equal(&self, limit: usize) -> usize {
        let mut s0 = 0;
        while s0 < limit && self.prev.hash[s0] == self.cur.hash[s0] {
            s0 += 1;
        }
        s0
    }

    /// Rows pinned above a block that moved up by `k` (a header that did not
    /// move): at most the leading run of rows equal at the same position. A
    /// trailing row that also matches the shifted position (usually blank) is
    /// resolved toward the pin the layout showed last time.
    fn pinned_rows_up(&self, top_cur: usize, k: usize) -> usize {
        let s_max = self.leading_equal(top_cur);
        let mut s_min = s_max;
        while s_min > 0 && self.cur.hash[s_min - 1] == self.prev.hash[s_min - 1 + k] {
            s_min -= 1;
        }
        self.pin.clamp(s_min, s_max)
    }

    /// Rule 6: the content moved up by `k`; `top_cur` is where the topmost
    /// voting anchor landed.
    fn shift_up(&mut self, k: usize, top_cur: usize) {
        let s0 = self.pinned_rows_up(top_cur, k);
        // Content growing up into an empty top area: the rows that "left" are
        // blank rows of an area still there, so nothing scrolled off.
        if (s0..s0 + k).all(|i| self.prev.blank(i)) && self.cur.blank(s0) {
            return;
        }
        self.pin = s0;
        // Rows the screen was re-showing from the archive are skipped — each one
        // only if it is the archived row it is assumed to be (a duplicate beats a
        // loss, so the first that is not ends the skipping).
        let mut r = 0;
        while r < k.min(self.debt)
            && self.archived_hash(self.debt_at + r as u64) == Some(self.prev.hash[s0 + r])
        {
            r += 1;
        }
        if r == k && self.debt >= k {
            self.debt_at += k as u64;
            // `check_debt` re-measures it on this frame: the screen may go on
            // re-showing archived rows past what the old run could see.
            self.debt = if self.debt_at <= self.last() {
                (self.debt - k).max(1)
            } else {
                0
            };
        } else {
            self.debt = 0;
        }
        self.scratch.picks.clear();
        self.scratch.picks.extend(s0 + r..s0 + k);
        self.archive_picks();
    }

    /// Rule 7: the content moved down by `j` over `[0, t)`. Rows revealed at
    /// the top are the archived rows just before the old top (the debt). Rows
    /// pushed out at the bottom below the block's lowest voter wait in `below`
    /// until they re-enter or a flush archives them (a duplicate beats a loss).
    fn shift_down(&mut self, v: Vote, t: usize) {
        let j = v.d.unsigned_abs();
        let (top_prev, bot_prev) = (v.top - j, v.bot - j);
        let (dlo, dhi) = (self.pin, self.pin + self.debt);
        let mut leaving = std::mem::take(&mut self.scratch.picks);
        leaving.clear();
        leaving.extend((bot_prev + 1..t).filter(|&p| {
            let archived = p >= dlo && p < dhi;
            let landed = p + j < t && self.cur.hash[p + j] == self.prev.hash[p];
            !archived && !landed
        }));
        for &p in leaving.iter().rev() {
            let row = self.prev_row(p);
            self.below.push_front(row);
        }
        self.scratch.picks = leaving;
        // Bounded: only rows displayed and never archived get here (a screen's
        // worth before the debt covers the whole screen), plus a changed status
        // row per frame; past this many screens the deepest are dropped.
        self.below.truncate(REANCHOR_SCREENS * self.cur.rows());
        self.pin = self.pin.min(self.leading_equal(top_prev));
        // The revealed rows are the j archived rows just before the screen's
        // old top (the archive's end when nothing was re-shown) — never rows of
        // an earlier app run.
        let base = if self.debt > 0 {
            self.debt_at
        } else {
            self.last() + 1
        };
        if base >= self.first.max(self.floor) + j as u64 {
            self.debt_at = base - j as u64;
            self.debt += j;
        } else {
            // Older than what is retained or than this app run: unverifiable.
            // Those rows and the re-shown ones are archived again as they scroll
            // off, after a recorded gap.
            self.debt = 0;
            self.push_gap(AltArchiveGapKind::Jump);
        }
    }

    /// Rows waiting in `below` that are back on screen: the deepest one found
    /// (an anchor, unique on screen) re-entered, and every row above it with it.
    fn settle_below(&mut self) {
        if self.below.is_empty() {
            return;
        }
        let s = &self.scratch;
        let back = self.below.iter().rposition(|r| {
            r.anchor
                && s.cur_idx
                    .get(&r.hash)
                    .is_some_and(|&(n, i)| n == 1 && *self.cur.text[i] == *r.text)
        });
        if let Some(m) = back {
            self.below.drain(..=m);
        }
    }

    /// `prev[i]` as an archived row.
    fn prev_row(&self, i: usize) -> ArchivedRow {
        ArchivedRow {
            text: Arc::from(self.prev.text[i].as_str()),
            hash: self.prev.hash[i],
            anchor: self.prev.anchor[i],
        }
    }

    /// Rule 8, the jump: flush what was displayed, record the gap, reanchor.
    fn jump(&mut self, t: usize) {
        // Rows still on screen at the same position (the leading run) are not
        // lost: they stay tracked in the new frame.
        let q = self.leading_equal(t);
        self.flush_prev_rows(q, t);
        self.push_gap(AltArchiveGapKind::Jump);
        self.debt = 0;
        if let Some(at) = self.reanchor(t) {
            // Provisional: `check_debt` measures the real run from here.
            self.debt_at = at;
            self.debt = 1;
        }
    }

    /// Rule 8's reanchor: the index of the archived row the new screen shows
    /// at row `pin`, by a vote of the screen's anchors; `None` when it is
    /// nowhere in the recent archive.
    fn reanchor(&mut self, t: usize) -> Option<u64> {
        let len = self.rows.len();
        if len == 0 || t <= self.pin {
            return None;
        }
        // Never into an earlier app run (see `floor`).
        let floor = usize::try_from(self.floor.saturating_sub(self.first)).unwrap_or(usize::MAX);
        let lo = len
            .saturating_sub(REANCHOR_SCREENS * self.cur.rows())
            .max(floor);
        let s = &mut self.scratch;
        s.tail_idx.clear();
        for pos in lo..len {
            let row = &self.rows[pos];
            if row.anchor {
                s.tail_idx.insert(row.hash, pos); // the most recent occurrence wins
            }
        }
        s.j_votes.clear();
        for i in self.pin..t {
            if !self.cur.anchor[i] {
                continue;
            }
            let h = self.cur.hash[i];
            if s.cur_idx.get(&h).is_none_or(|e| e.0 != 1) {
                continue;
            }
            let Some(&pos) = s.tail_idx.get(&h) else {
                continue;
            };
            if *self.rows[pos].text != *self.cur.text[i] {
                continue;
            }
            let off = i - self.pin;
            if pos >= off {
                *s.j_votes.entry(pos - off).or_insert(0) += 1;
            }
        }
        s.j_votes
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
            .map(|(&j, _)| self.first + j as u64)
    }

    /// Hash of the retained archived row with absolute index `index`.
    fn archived_hash(&self, index: u64) -> Option<u64> {
        let off = usize::try_from(index.checked_sub(self.first)?).ok()?;
        self.rows.get(off).map(|r| r.hash)
    }

    /// Rule 9: re-measure the debt as the run of screen rows from `pin` that
    /// are consecutive archived rows from `debt_at`. A misaligned top ends it; a
    /// differing row further down cuts it there. Rows skipped on the debt's word
    /// are also verified individually in [`shift_up`](Self::shift_up).
    fn check_debt(&mut self, t: usize) {
        if self.debt == 0 {
            return;
        }
        let mut n = 0;
        while self.pin + n < t
            && self.archived_hash(self.debt_at + n as u64) == Some(self.cur.hash[self.pin + n])
        {
            n += 1;
        }
        self.debt = n;
    }

    /// Archive the rows of `prev` in `[from, to)` that are not in the debt
    /// region (already archived), then the rows waiting in `below` (they were
    /// displayed under those), trimming blank edges: a flush is always followed
    /// by a gap.
    fn flush_prev_rows(&mut self, from: usize, to: usize) {
        let to = to.min(self.prev.rows());
        let (dlo, dhi) = (self.pin, self.pin + self.debt);
        let mut out: Vec<ArchivedRow> = (from..to)
            .filter(|&i| i < dlo || i >= dhi)
            .map(|i| self.prev_row(i))
            .collect();
        out.extend(self.below.drain(..));
        let mut lo = 0;
        let mut hi = out.len();
        if self.at_boundary() {
            while lo < hi && out[lo].text.is_empty() {
                lo += 1;
            }
        }
        while hi > lo && out[hi - 1].text.is_empty() {
            hi -= 1;
        }
        for row in out.drain(lo..hi) {
            self.bytes += row.cost();
            self.rows.push_back(row);
        }
        self.evict();
    }

    /// Append `prev[i]` for every `i` in `scratch.picks` (rows a shift scrolled
    /// off). Leading blank rows are dropped when they would open the archive or
    /// follow a gap (they separate nothing).
    fn archive_picks(&mut self) {
        let picks = std::mem::take(&mut self.scratch.picks);
        let mut lo = 0;
        let hi = picks.len();
        if self.at_boundary() {
            while lo < hi && self.prev.blank(picks[lo]) {
                lo += 1;
            }
        }
        for &i in &picks[lo..hi] {
            let row = self.prev_row(i);
            self.bytes += row.cost();
            self.rows.push_back(row);
        }
        self.scratch.picks = picks;
        self.evict();
    }

    /// Nothing precedes the next row on any screen: the archive never held a
    /// row, or the last thing recorded is a gap.
    fn at_boundary(&self) -> bool {
        let last = self.last();
        last == 0 || self.gaps.back().is_some_and(|g| g.after == last)
    }

    fn push_gap(&mut self, kind: AltArchiveGapKind) {
        let after = self.last();
        if after == 0 || self.gaps.back().is_some_and(|g| g.after == after) {
            return;
        }
        self.gaps.push_back(AltArchiveGap { after, kind });
    }

    /// Evict the oldest rows until this archive is inside what it may retain —
    /// its own budget, and its share of any pool it joined. The share falls as
    /// sessions open, so the same archive can be over a budget it never set.
    fn evict(&mut self) {
        // Count this archive before reading the share: its new rows are
        // already in, and leaving itself out of the divisor would over-grant.
        self.sync_share();
        let budget = self.effective_budget();
        while self.bytes > budget || self.rows.len() > self.max_rows {
            let Some(row) = self.rows.pop_front() else {
                break;
            };
            self.bytes -= row.cost();
            self.first += 1;
            self.lost += 1;
        }
        // The trim can have emptied it (a share below one row's cost, floored
        // at `min_share`): give the share back in the same call.
        self.sync_share();
        // A gap after `g` matters to a reader holding row `g`; once `g + 1` is
        // gone too, `lost` already says everything.
        while self.gaps.front().is_some_and(|g| g.after + 1 < self.first) {
            self.gaps.pop_front();
        }
        if self.debt > 0 && self.debt_at < self.first {
            self.debt = 0; // the re-shown rows were evicted: nothing left to skip
        }
    }

    // --------------------------------------------------- lifecycle events

    /// The app left the alternate screen: flush the last committed frame's
    /// scrolling region (not the chrome) and the rows it pushed out at the
    /// bottom, record the gap, and drop the baseline. The caller commits the
    /// alt grid first, while it still exists.
    pub fn leave(&mut self) {
        self.end_run(false, AltArchiveGapKind::Leave);
    }

    /// [`leave`](Self::leave) for a screen no app is behind any more (a shell
    /// prompt arrived on it, or the host forced the primary screen): flush the
    /// whole last frame, chrome included. The app is gone, so the rows measured
    /// as chrome may be the shell's; an extra row is a duplicate, never a loss.
    pub fn leave_whole(&mut self) {
        self.end_run(true, AltArchiveGapKind::Leave);
    }

    /// The app entered the alternate screen: a new baseline (no rows), and a
    /// new run — nothing archived before it is this screen's to re-show.
    pub fn enter(&mut self) {
        if self.enabled {
            self.new_run();
        }
    }

    /// The grids were replaced wholesale (checkpoint restore): flush, gap,
    /// new baseline.
    pub fn restore(&mut self) {
        self.end_run(false, AltArchiveGapKind::Restore);
    }

    /// Flush the last committed frame — its scrolling region, or the `whole`
    /// frame — record a `kind` gap, and start a new run.
    fn end_run(&mut self, whole: bool, kind: AltArchiveGapKind) {
        if !self.enabled {
            return;
        }
        if self.have_prev {
            let t = if whole {
                self.prev.rows()
            } else {
                self.prev_region()
            };
            self.flush_prev_rows(0, t);
        }
        self.push_gap(kind);
        self.new_run();
    }

    /// A new run: a new epoch, no baseline, and nothing archived before it is
    /// the next screen's to re-show.
    fn new_run(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.drop_prev();
        self.floor = self.last() + 1;
    }

    /// Install the frame in the next-frame buffers (filled through
    /// [`fill_from_grid`](Self::fill_from_grid)) as the baseline without diffing
    /// it: a checkpoint-restored screen continues nothing this archive saw but is
    /// what the app's next frame changes. A blank screen is no frame (see
    /// [`commit_prepared`](Self::commit_prepared)).
    fn install_restored(&mut self, cols: u16, key: Option<u64>) {
        if !self.enabled || self.cur.rows() == 0 || self.cur.all_blank() {
            return;
        }
        self.drop_prev();
        self.cur.cols = cols;
        self.cur.derive();
        self.cur.derive_votes();
        self.install_cur(key);
    }

    /// A full reset: every retained row is discarded (counted in `lost`), a
    /// `reset` gap marks the spot, and the baseline is dropped. Indices keep
    /// increasing.
    pub fn wipe(&mut self) {
        let n = self.rows.len() as u64;
        self.lost += n;
        self.first += n;
        self.rows.clear();
        self.bytes = 0;
        self.sync_share();
        self.gaps.clear();
        let last = self.last();
        if last > 0 {
            self.gaps.push_back(AltArchiveGap {
                after: last,
                kind: AltArchiveGapKind::Reset,
            });
        }
        self.new_run();
    }

    // --------------------------------------------------- handoff carry

    /// Where the archive stands now (see [`AltArchiveFence`]).
    #[must_use]
    pub fn fence(&self) -> AltArchiveFence {
        AltArchiveFence {
            origin: self.origin,
            epoch: self.epoch,
            first: self.first,
            last: self.last(),
            lost: self.lost,
            gaps: self.gaps.len(),
            differ: self.differ_fingerprint(),
        }
    }

    /// The carry a handoff takes inside its freeze: the counters, the gap at
    /// the newest row and, with `differ`, the differ's screen-sized state. No
    /// rows: they count in `lost` until [`carry_rows`](Self::carry_rows) attaches
    /// them. On its own it is a valid counters-only carry.
    #[must_use]
    pub fn carry_head(&self, differ: bool) -> (AltArchiveFence, AltArchiveCarry) {
        let fence = self.fence();
        let carry = AltArchiveCarry {
            origin: self.origin,
            first: fence.last + 1,
            lost: self.lost + self.rows.len() as u64,
            floor: self.floor,
            epoch: self.epoch,
            enabled: self.enabled,
            gaps: self
                .gaps
                .back()
                .filter(|g| g.after == fence.last)
                .copied()
                .into_iter()
                .collect(),
            rows: Vec::new(),
            differ: differ.then(|| self.differ_state()),
        };
        (fence, carry)
    }

    /// A fingerprint of exactly what [`differ_state`](Self::differ_state)
    /// takes, from the hashes the differ already keeps: no allocation, a few
    /// hundred words hashed, so the freeze pays for it inside the fence.
    fn differ_fingerprint(&self) -> u64 {
        let mut h = FxHasher::default();
        h.write_u8(u8::from(self.have_prev));
        if self.have_prev {
            h.write_u16(self.prev.cols);
            h.write_usize(self.prev.rows());
            for &row in &self.prev.hash {
                h.write_u64(row);
            }
        }
        match self.chrome {
            Some(c) => {
                h.write_u8(1);
                h.write_usize(c);
            }
            None => h.write_u8(0),
        }
        h.write_usize(self.pin);
        h.write_usize(self.debt);
        // Meaningless without a debt (and left stale by `drop_prev`).
        if self.debt > 0 {
            h.write_u64(self.debt_at);
        }
        h.write_usize(self.below.len());
        for row in &self.below {
            h.write_u64(row.hash);
        }
        h.finish()
    }

    /// The differ's state as [`AltArchiveDiffer`] (its `esu_seen` belongs to
    /// the terminal hook, which fills it in).
    fn differ_state(&self) -> AltArchiveDiffer {
        AltArchiveDiffer {
            prev: self
                .have_prev
                .then(|| (self.prev.cols, self.prev.text.clone())),
            chrome: self.chrome,
            pin: self.pin,
            debt: self.debt,
            debt_at: self.debt_at,
            below: self.below.iter().map(|r| r.text.to_string()).collect(),
            esu_seen: false,
        }
    }

    /// Attach the differ's state to a carry whose head was taken without it,
    /// only while the archive and the differ still stand at `fence`, so the
    /// adopting differ continues exactly ([`AltArchiveImport::Exact`]). `false`,
    /// with `carry` untouched, when either moved or the carry already has one.
    pub fn carry_differ(&self, carry: &mut AltArchiveCarry, fence: AltArchiveFence) -> bool {
        if carry.differ.is_some() || self.fence() != fence || carry.origin != fence.origin {
            return false;
        }
        carry.differ = Some(self.differ_state());
        true
    }

    /// The oldest row the adopting differ may still point at (the last
    /// [`REANCHOR_SCREENS`] screens and the re-shown run from `debt_at`, never
    /// before this app run's `floor`), so a carry always reaches back to it.
    fn carry_reach(&self) -> u64 {
        let screen = self.prev.rows().max(self.cur.rows());
        let span = u64::try_from(REANCHOR_SCREENS * screen).unwrap_or(u64::MAX);
        let mut reach = (self.last() + 1).saturating_sub(span).max(self.floor);
        if self.debt > 0 {
            reach = reach.min(self.debt_at);
        }
        reach
    }

    /// Attach the retained rows from index `from` on to a carry taken by
    /// [`carry_head`](Self::carry_head), never fewer than the differ's reach: the
    /// newest of them within `max_bytes` (charged like the budget; older ones
    /// count in `lost`), with every gap they need. `false`, with `carry`
    /// untouched, when the rows moved past `fence`. One `Arc` clone per row.
    pub fn carry_rows(
        &self,
        carry: &mut AltArchiveCarry,
        fence: AltArchiveFence,
        from: u64,
        max_bytes: usize,
    ) -> bool {
        if self.fence().rows_part() != fence.rows_part() || carry.origin != fence.origin {
            return false;
        }
        let end = fence.last + 1;
        let start = from.min(self.carry_reach()).clamp(self.first, end);
        let mut lo = end;
        let mut bytes = 0usize;
        while lo > start {
            let Some(text) = self.row(lo - 1) else {
                break;
            };
            let cost = alt_archive_row_charge(text.len());
            if bytes + cost > max_bytes {
                break;
            }
            bytes += cost;
            lo -= 1;
        }
        let skip = usize::try_from(lo - self.first).unwrap_or(usize::MAX);
        carry.rows = self
            .rows
            .iter()
            .skip(skip)
            .map(|r| Arc::clone(&r.text))
            .collect();
        carry.first = lo;
        carry.lost = self.lost + (lo - self.first);
        carry.gaps = self
            .gaps
            .iter()
            .filter(|g| g.after + 1 >= lo)
            .copied()
            .collect();
        true
    }

    /// Install a carry from the process this session was handed over from, in
    /// place of what this archive holds (called right after the checkpoint
    /// restore).
    ///
    /// Never panics and never leaves an out-of-range state: impossible indices
    /// refuse the carry; rows are normalized and re-hashed; gaps are sorted into
    /// range; the differ's state is taken whole or not at all
    /// ([`AltArchiveImport::NoBaseline`]). The own budget then evicts what it
    /// cannot hold. An archive that is off refuses the carry.
    pub fn import(&mut self, carry: AltArchiveCarry) -> AltArchiveImport {
        if !self.enabled {
            return AltArchiveImport::Refused;
        }
        let first = carry.first.max(1);
        let Some(end) = first
            .checked_add(carry.rows.len() as u64)
            .filter(|&end| end <= MAX_CARRIED_INDEX)
        else {
            return AltArchiveImport::Refused;
        };
        let last = end - 1;
        if carry.origin != 0 {
            self.origin = carry.origin;
        }
        self.rows.clear();
        self.bytes = 0;
        for text in carry.rows {
            let text = if is_normalized(&text) {
                text
            } else {
                let mut s = text.to_string();
                normalize_row(&mut s);
                Arc::from(s)
            };
            let row = ArchivedRow {
                hash: row_hash(&text),
                anchor: is_anchor_text(&text),
                text,
            };
            self.bytes += row.cost();
            self.rows.push_back(row);
        }
        self.first = first;
        self.lost = carry.lost.min(first - 1);
        self.epoch = carry.epoch;
        self.floor = carry.floor.clamp(1, end);
        let mut gaps: Vec<AltArchiveGap> = carry
            .gaps
            .into_iter()
            .filter(|g| g.after >= 1 && g.after <= last)
            .collect();
        gaps.sort_by_key(|g| g.after);
        gaps.dedup_by_key(|g| g.after);
        self.gaps = gaps.into();
        let differ = carry
            .differ
            .filter(|d| carry.enabled && differ_fits(d, last));
        let outcome = match differ {
            Some(d) => {
                self.install_differ(d);
                AltArchiveImport::Exact
            }
            None => {
                self.drop_prev();
                AltArchiveImport::NoBaseline
            }
        };
        self.evict();
        outcome
    }

    /// Install a differ state [`differ_fits`] accepted.
    fn install_differ(&mut self, d: AltArchiveDiffer) {
        self.drop_prev();
        let Some((cols, rows)) = d.prev else {
            return;
        };
        self.prev.text = rows;
        self.prev.cols = cols;
        self.prev.derive();
        self.prev.derive_votes();
        self.have_prev = true;
        self.chrome = d.chrome;
        self.pin = d.pin;
        self.debt = d.debt;
        self.debt_at = d.debt_at;
        self.below = d
            .below
            .into_iter()
            .map(|s| ArchivedRow {
                hash: row_hash(&s),
                anchor: is_anchor_text(&s),
                text: Arc::from(s),
            })
            .collect();
    }
}

/// Whether a carried differ state describes a frame the differ can go on
/// from: a non-blank frame of normalized rows at a real width; chrome, pin
/// and the re-shown run inside it; the re-shown rows inside the carried
/// indices (rows left out of the carry only end the run, as eviction does);
/// `below` normalized and within its bound. No frame means no state at all.
fn differ_fits(d: &AltArchiveDiffer, last: u64) -> bool {
    let Some((cols, rows)) = &d.prev else {
        return d.chrome.is_none() && d.pin == 0 && d.debt == 0 && d.below.is_empty();
    };
    let n = rows.len();
    *cols > 0
        && n > 0
        && u16::try_from(n).is_ok()
        && rows.iter().all(|r| is_normalized(r))
        && rows.iter().any(|r| !r.is_empty())
        && d.chrome.is_none_or(|c| c <= n)
        && d.pin <= n
        && (d.debt == 0
            || (d.pin.checked_add(d.debt).is_some_and(|e| e <= n)
                && d.debt_at >= 1
                && d.debt_at
                    .checked_add(d.debt as u64)
                    .is_some_and(|e| e <= last + 1)))
        && d.below.len() <= REANCHOR_SCREENS * n
        && d.below.iter().all(|r| is_normalized(r))
}

/// Whether `s` is already what [`normalize_row`] makes of it.
fn is_normalized(s: &str) -> bool {
    s.len() == s.trim_end().len() && !s.chars().any(|c| c == '\0' || c.is_control())
}

/// The winner of an anchor vote: content offset `d = pos_prev - pos_cur`, its
/// votes, and the cur positions of its topmost and lowest voters.
#[derive(Debug, Clone, Copy, Default)]
struct Vote {
    d: isize,
    votes: usize,
    top: usize,
    bot: usize,
}

impl Vote {
    /// More votes wins; a tie goes to the smaller move, then to the upward one.
    fn beats(self, other: Self) -> bool {
        self.votes > other.votes
            || (self.votes == other.votes
                && (self.d.unsigned_abs() < other.d.unsigned_abs()
                    || (self.d.unsigned_abs() == other.d.unsigned_abs() && self.d > other.d)))
    }
}

/// Index the eligible rows of `[0, t)` by hash: (occurrences, first position).
fn index_anchors(
    hash: &[u64],
    eligible: &[bool],
    t: usize,
    idx: &mut FxHashMap<u64, (u32, usize)>,
) {
    idx.clear();
    for i in 0..t {
        if eligible[i] {
            idx.entry(hash[i])
                .and_modify(|e| e.0 += 1)
                .or_insert((1, i));
        }
    }
}

fn row_hash(s: &str) -> u64 {
    let mut h = FxHasher::default();
    h.write(s.as_bytes());
    h.write_usize(s.len());
    h.finish()
}

/// Box-drawing / rule chars (and space): a row made only of these never votes.
fn is_rule_char(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '─'
            | '━'
            | '═'
            | '│'
            | '┃'
            | '║'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
            | '┌'
            | '┐'
            | '└'
            | '┘'
            | '├'
            | '┤'
            | '┬'
            | '┴'
            | '┼'
    )
}

/// What a row says: its chars with box/rule art and spaces removed.
fn said_text(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().filter(|&ch| !is_rule_char(ch))
}

/// A row that says something: not blank, not only rules/box art.
fn is_content_text(s: &str) -> bool {
    s.chars().any(|ch| !is_rule_char(ch))
}

/// A row distinctive enough to vote: at least [`MIN_ANCHOR_CHARS`] visible chars
/// after trimming, not only rules/box art. (Uniqueness is checked per frame.)
fn is_anchor_text(s: &str) -> bool {
    let mut n = 0;
    let mut non_rule = false;
    for ch in s.trim().chars() {
        n += 1;
        non_rule |= !is_rule_char(ch);
        if n >= MIN_ANCHOR_CHARS && non_rule {
            return true;
        }
    }
    false
}

/// Normalize extracted row text to exactly what `text` shows: NUL/control chars
/// collapse to a space (the control plane's `visible_char`, applied here so the
/// hash and the archived bytes agree with the screen reply; applying it again on
/// output is idempotent), then trailing whitespace is trimmed.
fn normalize_row(s: &mut String) {
    // Fast screen: C0, DEL, and the UTF-8 lead byte of U+0080..=U+00BF (C1
    // controls live there, alongside common glyphs like `·`).
    if s.bytes().any(|b| b < 0x20 || b == 0x7f || b == 0xc2)
        && s.chars().any(|c| c == '\0' || c.is_control())
    {
        let mapped: String = s
            .chars()
            .map(|c| if c == '\0' || c.is_control() { ' ' } else { c })
            .collect();
        *s = mapped;
    }
    let end = s.trim_end().len();
    s.truncate(end);
}

/// The first index of `needle` in `hay`. The matcher runs over every byte the
/// terminal receives, beside a parser that scans them in bulk, so it compares
/// 64 bytes per step (a fixed-size block the compiler vectorizes) and walks
/// bytes only inside the block that holds a hit.
fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    let (blocks, tail) = hay.as_chunks::<64>();
    for (n, block) in blocks.iter().enumerate() {
        let mut any = 0u8;
        for &b in block {
            any |= u8::from(b == needle);
        }
        if any != 0 {
            return block.iter().position(|&b| b == needle).map(|i| n * 64 + i);
        }
    }
    let base = blocks.len() * 64;
    tail.iter().position(|&b| b == needle).map(|i| base + i)
}

/// Whether `input` holds conhost's alt-screen enter, `ESC [ ? 1049 h`. It runs
/// over every read while `TransientState::conhost_alt_screen_left_up` is set —
/// for a cmd.exe session that never starts another full-screen app, the rest of
/// the session — so it hops from ESC to ESC with [`find_byte`] instead of
/// comparing a window at every byte.
fn has_alt_1049_enter(input: &[u8]) -> bool {
    const ENTER: &[u8] = b"\x1b[?1049h";
    let mut rest = input;
    while let Some(at) = find_byte(rest, 0x1b) {
        if rest[at..].starts_with(ENTER) {
            return true;
        }
        rest = &rest[at + 1..];
    }
    false
}

// The streaming DEC-mode matcher: finds the `CSI ? Pm <final>` sequences that
// end a frame or switch screens, across read boundaries.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum MatchState {
    #[default]
    Ground,
    Esc,
    CsiEntry,
    Params,
}

/// Alt-screen mode bits a sequence's params named: 47, 1047, 1049.
const ALT_47: u8 = 1;
const ALT_1047: u8 = 2;
const ALT_1049: u8 = 4;

/// Where the hook splits the parser's input around a matched sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HitKind {
    /// On the alt screen: an ESU (`CSI ? … 2026 … l`) — split after it and
    /// commit the frame it closed.
    Esu,
    /// On the alt screen: an alt-screen exit (`CSI ? 47/1047/1049 l`) — split
    /// one byte short of it and commit the alt grid while it still exists.
    Exit,
    /// On the alt screen: XTRESTORE of an alt-screen mode (`CSI ? 1049 r`),
    /// which exits when the saved value is "off" — split one byte short and
    /// commit first if it will. The mode bits it names.
    Restore(u8),
    /// On the main screen: an alt-screen enter (`CSI ? 1049 h`, or an XTRESTORE
    /// that may enter) — split after it so the frames that follow in the same
    /// read are each committed.
    Enter,
    /// On the alt screen: RIS (`ESC c`), which leaves it; split after it so the
    /// wipe lands where it happened and the scan continues for the main screen.
    Reset,
}

/// A sequence the hook must act on. `final_at` is the offset of its final byte
/// in the slice handed to [`DecModeMatcher::scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecModeHit {
    final_at: usize,
    kind: HitKind,
}

/// Streaming recognizer for DEC private mode sets, resets and XTRESTOREs. It
/// mirrors the VT500 parser's CSI grammar closely enough that its split points
/// are always at a sequence's final byte: C0 controls inside a CSI execute
/// without aborting it, can/SUB abort, ESC restarts, an intermediate or a
/// second private marker makes the sequence irrelevant. State survives across
/// calls, so a sequence straddling two PTY reads is still found.
///
/// It never walks byte by byte between sequences: on the alt screen it jumps
/// ESC to ESC; on the main screen, where only an alt-screen enter matters, it
/// jumps `?` to `?` and takes one only when `ESC [` precedes it (including the
/// previous read's last two bytes). An enter with a C0 control between `ESC [`
/// and `?` is found at the next commit point instead.
#[derive(Debug, Clone, Copy, Default)]
struct DecModeMatcher {
    state: MatchState,
    param: u32,
    esu: bool,
    alt: u8,
    /// The last two bytes scanned (the older first): an `ESC [` that ended the
    /// previous read still introduces a `?` that starts this one.
    tail: [u8; 2],
}

impl DecModeMatcher {
    fn finish_param(&mut self) {
        match self.param {
            2026 => self.esu = true,
            47 => self.alt |= ALT_47,
            1047 => self.alt |= ALT_1047,
            1049 => self.alt |= ALT_1049,
            _ => {}
        }
        self.param = 0;
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    /// What a completed `CSI ? … <final>` means for the hook, given the screen.
    fn classify(&self, final_byte: u8, on_alt: bool) -> Option<HitKind> {
        match (final_byte, on_alt) {
            (b'l', true) if self.alt != 0 => Some(HitKind::Exit),
            (b'l', true) if self.esu => Some(HitKind::Esu),
            (b'r', true) if self.alt != 0 => Some(HitKind::Restore(self.alt)),
            (b'h' | b'r', false) if self.alt != 0 => Some(HitKind::Enter),
            _ => None,
        }
    }

    /// Advance over `bytes`, returning at the first sequence the hook acts on
    /// (its final byte consumed), or `None` with the state carried to the next
    /// call. `on_alt`: whether the alternate screen is active.
    fn scan(&mut self, bytes: &[u8], on_alt: bool) -> Option<DecModeHit> {
        let hit = self.scan_inner(bytes, on_alt);
        let seen = hit.map_or(bytes.len(), |h| h.final_at + 1);
        self.tail = match seen {
            0 => self.tail,
            1 => [self.tail[1], bytes[0]],
            n => [bytes[n - 2], bytes[n - 1]],
        };
        hit
    }

    /// Whether `ESC [` precedes offset `i` (the previous read's last bytes
    /// standing in for what `bytes` does not hold).
    fn csi_before(&self, bytes: &[u8], i: usize) -> bool {
        let at = |back: usize| -> u8 {
            if i >= back {
                bytes[i - back]
            } else {
                self.tail[2 - (back - i)]
            }
        };
        at(2) == 0x1b && at(1) == b'['
    }

    fn scan_inner(&mut self, bytes: &[u8], on_alt: bool) -> Option<DecModeHit> {
        let mut i = 0;
        while i < bytes.len() {
            if self.state == MatchState::Ground {
                if on_alt {
                    i += find_byte(&bytes[i..], 0x1b)? + 1;
                    self.state = MatchState::Esc;
                } else {
                    i += find_byte(&bytes[i..], b'?')?;
                    if self.csi_before(bytes, i) {
                        self.state = MatchState::Params;
                        self.param = 0;
                        self.esu = false;
                        self.alt = 0;
                    }
                    i += 1;
                }
                continue;
            }
            let b = bytes[i];
            match self.state {
                MatchState::Ground => {}
                MatchState::Esc => {
                    self.state = match b {
                        b'[' => MatchState::CsiEntry,
                        // C0 controls execute and stay; ESC restarts; can/SUB abort.
                        0x00..=0x17 | 0x19 | 0x1b..=0x1f => MatchState::Esc,
                        _ => MatchState::Ground,
                    };
                    if b == b'c' && on_alt {
                        return Some(DecModeHit {
                            final_at: i,
                            kind: HitKind::Reset,
                        });
                    }
                }
                MatchState::CsiEntry => match b {
                    b'?' => {
                        self.state = MatchState::Params;
                        self.param = 0;
                        self.esu = false;
                        self.alt = 0;
                    }
                    0x1b => self.state = MatchState::Esc,
                    0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => {}
                    _ => self.state = MatchState::Ground,
                },
                MatchState::Params => match b {
                    b'0'..=b'9' => {
                        self.param = (self.param * 10 + u32::from(b - b'0')).min(100_000);
                    }
                    b';' | b':' => self.finish_param(),
                    b'l' | b'h' | b'r' => {
                        self.finish_param();
                        self.state = MatchState::Ground;
                        if let Some(kind) = self.classify(b, on_alt) {
                            return Some(DecModeHit { final_at: i, kind });
                        }
                    }
                    0x1b => self.state = MatchState::Esc,
                    0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => {}
                    _ => self.state = MatchState::Ground,
                },
            }
            i += 1;
        }
        None
    }
}

// The Terminal hook.

/// The archive plus the hook's bookkeeping: one `Terminal` field, session-only,
/// never checkpointed (a handoff carries the archive via [`AltArchiveCarry`]).
///
/// The handler borrows it for one call, [`Self::leave_orphaned`]: an alternate
/// screen left from inside the parser's dispatch (an OSC 133 `A` arriving on
/// it), where no stream split can place a commit point before the swap. Every
/// other commit point is on the `Terminal` side.
#[derive(Debug)]
pub(super) struct AltArchiveState {
    archive: AltArchive,
    matcher: DecModeMatcher,
    /// `sync_end_seq` at the last commit point.
    seen_sync_seq: u64,
    /// `transient.reset_generation` at the last commit point.
    seen_reset_gen: u64,
    /// Which screen the archive believes is active.
    on_alt: bool,
    /// A 2026 close was committed in this alt epoch: the app paces its own
    /// frames, so the epilogue fallback stays out of its way.
    esu_seen: bool,
    last_fallback: Option<aterm_time::Instant>,
}

impl AltArchiveState {
    /// Enabled: the archive is on for every terminal.
    pub(super) fn new() -> Self {
        Self {
            archive: AltArchive::new(),
            matcher: DecModeMatcher::default(),
            seen_sync_seq: 0,
            seen_reset_gen: 0,
            on_alt: false,
            esu_seen: false,
            last_fallback: None,
        }
    }

    /// The alternate screen is about to be left without an app asking (a shell
    /// prompt, OSC 133/633 `A`, arrived on it, or the host forced the primary
    /// screen). `grid` is the still-active alt grid: commit it, flush it whole
    /// ([`AltArchive::leave_whole`]) and mark the screen left so the epilogue
    /// does not `leave` again. `sync_end_seq` is the batch's current value.
    pub(super) fn leave_orphaned(&mut self, grid: &Grid, sync_end_seq: u64) {
        if !self.archive.enabled {
            return;
        }
        self.archive.commit_grid(grid);
        self.archive.leave_whole();
        self.on_alt = false;
        self.esu_seen = false;
        self.last_fallback = None;
        self.seen_sync_seq = sync_end_seq;
    }
}

impl Terminal {
    /// The alt-screen scroll-off archive (read-only). Call
    /// [`AltArchive::read`] under the terminal lock and format afterwards.
    #[must_use]
    pub fn alt_archive(&self) -> &AltArchive {
        &self.alt_archive.archive
    }

    /// Clone rows out of the alt-screen archive (see [`AltArchive::read`]).
    #[must_use]
    pub fn alt_archive_read(&self, q: AltArchiveQuery) -> AltArchiveRead {
        self.alt_archive.archive.read(q)
    }

    /// Set the archive's origin: a value unique to this process so indices are
    /// never compared across a restart, or across a handoff that could not
    /// carry the archive (one that does keeps the carried origin).
    pub fn set_alt_archive_origin(&mut self, origin: u64) {
        self.alt_archive.archive.set_origin(origin);
    }

    /// Draw on the process's shared archive budget
    /// ([`AltArchiveBudget::process`]) instead of a per-session one, so live
    /// sessions share [`ALT_ARCHIVE_TOTAL_BUDGET`]. Dropping the terminal gives
    /// the share back. Hosts with one session per process need not call it.
    pub fn set_alt_archive_shared(&mut self, shared: bool) {
        self.alt_archive
            .archive
            .share_budget(shared.then(AltArchiveBudget::process));
    }

    /// Set the archive's byte budget; 0 turns it off and wipes it. A session
    /// sharing the process budget retains the smaller of this and its share.
    pub fn set_alt_archive_budget(&mut self, budget: usize) {
        let was = self.alt_archive.archive.enabled;
        self.alt_archive.archive.set_budget(budget);
        if !was && self.alt_archive.archive.enabled {
            self.alt_archive_resync();
        }
    }

    /// Turn the archive on or off for this session. Off wipes it.
    pub fn set_alt_archive_enabled(&mut self, enabled: bool) {
        let was = self.alt_archive.archive.enabled;
        self.alt_archive.archive.set_enabled(enabled);
        if !was && enabled {
            self.alt_archive_resync();
        }
    }

    /// Start following from the terminal's current state (on enable).
    fn alt_archive_resync(&mut self) {
        let st = &mut self.alt_archive;
        st.matcher.reset();
        st.seen_sync_seq = self.transient.sync_end_seq;
        st.seen_reset_gen = self.transient.reset_generation;
        st.on_alt = self.modes.alternate_screen;
        st.esu_seen = false;
        st.last_fallback = None;
        st.archive.drop_prev();
    }

    /// Feed `input` through the parser, splitting after every ESU, one byte
    /// short of every alt-screen exit, and after an alt-screen enter, so the
    /// archive sees each committed frame and the alt grid before it is swapped
    /// away. Splitting does not change the grid (the parser is streaming;
    /// `alt_archive_claude_like` checks 1..17-byte chunks).
    pub(super) fn advance_parser_archiving(&mut self, input: &[u8]) {
        // A host-forced primary screen (`Terminal::leave_alternate_screen`) runs
        // here, before the batch's bytes, so it is treated like an in-band `?1049l`.
        if std::mem::take(&mut self.transient.host_leave_alternate_screen) {
            let (_, mut handler) = self.split_for_process();
            handler.leave_orphaned_alternate_screen();
        }
        // conhost, still on the alternate buffer an orphaned leave left it on,
        // sends its blank repaint of a new alt screen ahead of the `?1049h` in
        // the same read (see `TransientState::conhost_alt_screen_left_up`), so
        // the rows are kept before this read runs. An enter split across two
        // reads is not caught.
        if self.transient.conhost_alt_screen_left_up
            && !self.modes.alternate_screen
            && has_alt_1049_enter(input)
        {
            let (_, mut handler) = self.split_for_process();
            handler.keep_screen_from_conhost_repaint();
        }
        if !self.alt_archive.archive.enabled {
            self.advance_parser_raw(input);
            return;
        }
        let mut rest = input;
        while let Some(hit) = self
            .alt_archive
            .matcher
            .scan(rest, self.modes.alternate_screen)
        {
            let at = hit.final_at;
            let before = match hit.kind {
                HitKind::Exit => true,
                HitKind::Restore(modes) => self.xtrestore_leaves_alt(modes),
                HitKind::Esu | HitKind::Enter | HitKind::Reset => false,
            };
            if before {
                self.advance_parser_raw(&rest[..at]);
                self.alt_archive_before_exit();
                self.advance_parser_raw(&rest[at..=at]);
            } else {
                self.advance_parser_raw(&rest[..=at]);
            }
            self.alt_archive_boundary();
            rest = &rest[at + 1..];
        }
        self.advance_parser_raw(rest);
    }

    fn advance_parser_raw(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let (parser, mut handler) = self.split_for_process();
        parser.advance_fast(bytes, &mut handler);
    }

    /// Whether an XTRESTORE naming these alt-screen modes (`ALT_*` bits) turns
    /// the alternate screen off: one of them was saved as "off" (the handler
    /// restores each named mode whose saved value differs from the current).
    fn xtrestore_leaves_alt(&self, modes: u8) -> bool {
        [(ALT_47, 47u16), (ALT_1047, 1047), (ALT_1049, 1049)]
            .iter()
            .any(|&(bit, mode)| {
                modes & bit != 0 && self.transient.xtsave_modes.get(&mode) == Some(&false)
            })
    }

    /// A commit point: a reset since the last one wipes; a screen switch
    /// leaves/enters; a moved `sync_end_seq` (ESU, sync timeout, DECSTR) on the
    /// alt screen commits the frame.
    fn alt_archive_boundary(&mut self) {
        if !self.alt_archive.archive.enabled {
            return;
        }
        if self.transient.reset_generation != self.alt_archive.seen_reset_gen {
            let st = &mut self.alt_archive;
            st.seen_reset_gen = self.transient.reset_generation;
            st.archive.wipe();
            st.on_alt = false;
            st.esu_seen = false;
        }
        let alt = self.modes.alternate_screen;
        let seq = self.transient.sync_end_seq;
        if self.alt_archive.on_alt != alt {
            let st = &mut self.alt_archive;
            if alt {
                st.archive.enter();
            } else {
                st.archive.leave();
            }
            st.on_alt = alt;
            st.esu_seen = false;
            st.last_fallback = None;
            // A close that came before the switch belonged to the other screen
            // (the main screen's frames are not split at their ESUs); it neither
            // commits nor marks this alt run as 2026-paced.
            st.seen_sync_seq = seq;
            return;
        }
        if seq != self.alt_archive.seen_sync_seq {
            self.alt_archive.seen_sync_seq = seq;
            if alt {
                self.alt_archive.esu_seen = true;
                self.alt_archive_commit();
            }
        }
    }

    /// One byte before an alt-screen exit's final byte: settle any pending
    /// commit point, then commit the alt grid while it still exists.
    fn alt_archive_before_exit(&mut self) {
        self.alt_archive_boundary();
        if self.modes.alternate_screen {
            self.alt_archive_commit();
        }
    }

    /// The `process_at` epilogue (both branches, after `post_process`): pick up
    /// a sync-timeout close, and commit apps that never use 2026 (vim, less,
    /// htop) at most once per 16 ms — never while a 2026 window is open.
    pub(super) fn alt_archive_epilogue(&mut self) {
        if !self.alt_archive.archive.enabled {
            return;
        }
        self.alt_archive_boundary();
        if !self.modes.alternate_screen
            || self.alt_archive.esu_seen
            || self.modes.synchronized_output
        {
            return;
        }
        let now = self.transient.process_now;
        let due = self.alt_archive.last_fallback.is_none_or(|t| {
            now.checked_duration_since(t)
                .is_none_or(|d| d >= FALLBACK_COMMIT_INTERVAL)
        });
        if due {
            self.alt_archive.last_fallback = Some(now);
            self.alt_archive_commit();
        }
    }

    /// Commit the active (alt) grid's rows to the archive. Every caller runs
    /// only while the archive is enabled.
    fn alt_archive_commit(&mut self) {
        self.alt_archive.archive.commit_grid(&self.grid);
    }

    /// `restore_checkpoint` replaced the grids and modes: flush, gap and, on the
    /// alternate screen, install the restored screen as the new baseline so the
    /// app's next frame is diffed against it. Not when captured inside an open
    /// 2026 window: a half-painted frame would be a false baseline.
    pub(super) fn alt_archive_after_restore(&mut self) {
        let st = &mut self.alt_archive;
        st.archive.restore();
        st.seen_sync_seq = self.transient.sync_end_seq;
        st.seen_reset_gen = self.transient.reset_generation;
        st.on_alt = self.modes.alternate_screen;
        st.esu_seen = false;
        st.last_fallback = None;
        self.alt_archive_install_screen();
    }

    /// On the alternate screen, and not inside an open 2026 window: install
    /// the screen as the archive's baseline without diffing it (see
    /// [`Terminal::alt_archive_after_restore`]).
    fn alt_archive_install_screen(&mut self) {
        let st = &mut self.alt_archive;
        if !st.archive.enabled || !self.modes.alternate_screen || self.modes.synchronized_output {
            return;
        }
        let grid = &self.grid;
        let (rows, cols) = (grid.rows(), grid.cols());
        if rows == 0 || cols == 0 {
            return;
        }
        st.archive.fill_from_grid(grid);
        st.archive.install_restored(cols, Some(grid.content_gen()));
    }

    /// A self-update handoff's capture of this session's archive, taken inside
    /// the freeze under the checkpoint's lock: the fence and the counters-only
    /// carry plus, with `differ`, the differ's state. The rows (and a skipped
    /// differ state) are attached later ([`Terminal::alt_archive_carry_rows`],
    /// [`Terminal::alt_archive_carry_differ`]).
    #[must_use]
    pub fn alt_archive_carry_head(&self, differ: bool) -> (AltArchiveFence, AltArchiveCarry) {
        let (fence, mut carry) = self.alt_archive.archive.carry_head(differ);
        if let Some(d) = carry.differ.as_mut() {
            d.esu_seen = self.alt_archive.esu_seen;
        }
        (fence, carry)
    }

    /// Attach the differ's state to a carry [`Terminal::alt_archive_carry_head`]
    /// took without it (see [`AltArchive::carry_differ`]): `false` when the
    /// archive or the differ moved since, or it has one already.
    pub fn alt_archive_carry_differ(
        &self,
        carry: &mut AltArchiveCarry,
        fence: AltArchiveFence,
    ) -> bool {
        let attached = self.alt_archive.archive.carry_differ(carry, fence);
        if attached && let Some(d) = carry.differ.as_mut() {
            d.esu_seen = self.alt_archive.esu_seen;
        }
        attached
    }

    /// Attach rows to a carry [`Terminal::alt_archive_carry_head`] took (see
    /// [`AltArchive::carry_rows`]): `false` when the archive moved since.
    pub fn alt_archive_carry_rows(
        &self,
        carry: &mut AltArchiveCarry,
        fence: AltArchiveFence,
        from: u64,
        max_bytes: usize,
    ) -> bool {
        self.alt_archive
            .archive
            .carry_rows(carry, fence, from, max_bytes)
    }

    /// Install a carried archive into an engine that just restored the
    /// checkpoint taken with it ([`AltArchive::import`]). Without a fitting
    /// differ state the restored screen becomes the baseline after a `restore`
    /// gap.
    pub fn alt_archive_import(&mut self, carry: AltArchiveCarry) -> AltArchiveImport {
        let esu_seen = carry.differ.as_ref().is_some_and(|d| d.esu_seen);
        let outcome = self.alt_archive.archive.import(carry);
        match outcome {
            AltArchiveImport::Exact => self.alt_archive.esu_seen = esu_seen,
            AltArchiveImport::NoBaseline => {
                // No baseline left to flush: this is the gap and the new run.
                self.alt_archive.archive.restore();
                self.alt_archive.esu_seen = false;
                self.alt_archive_install_screen();
            }
            AltArchiveImport::Refused => {}
        }
        outcome
    }

    /// `Terminal::reset()` (the host's direct reset, which never reaches
    /// `process_at`): wipe now, and forget any half-matched sequence along with
    /// the parser's.
    pub(super) fn alt_archive_after_host_reset(&mut self) {
        self.alt_archive.matcher.reset();
        self.alt_archive_boundary();
    }
}

#[cfg(test)]
#[path = "alt_archive_tests.rs"]
mod tests;
