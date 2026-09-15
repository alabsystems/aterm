// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The alt-screen scroll-off archive: the rows a fullscreen app scrolled away.
//!
//! # Why
//!
//! A fullscreen app on the ALTERNATE screen (Claude Code, measured 2026-09-13)
//! repaints with absolute cursor moves and erases inside DEC 2026 synchronized
//! updates and emits no scroll sequence at all (a 4 MB capture: 0 DECSTBM, 0 SU/SD,
//! 0 IL/DL, 0 RI/IND, 0 LF). The alt grid has no scrollback by spec, so a row that
//! leaves the top of the screen is simply gone, and `lines` reads 0. In one measured
//! worker turn the worker displayed 35 message blocks and 7 were still readable.
//!
//! # What
//!
//! [`AltArchive`] is a PURE differ + store over committed frames: row text plus a
//! u64 row hash, no `Terminal` or handler coupling, unit-testable with `&[&str]`
//! frames. Between two committed frames it finds how far the content moved by
//! ANCHOR VOTING (rows that are distinctive and unique in both frames vote for the
//! offset that maps one onto the other) and archives exactly the rows that left the
//! top. Where it cannot tell, it prefers a duplicate to a loss: rows that were
//! displayed are flushed before a discontinuity, and every discontinuity is
//! recorded as a gap a reader can see.
//!
//! The Terminal side of this file is the hook: `process_at` splits its input after
//! every `CSI ? … 2026 … l` (ESU) with a streaming matcher that survives a sequence
//! straddling two PTY reads, and commits the alt grid when `sync_end_seq` moved.
//! The same matcher stops one byte short of an alt-screen EXIT so the alt grid can
//! be committed before the swap destroys it. Apps that never use 2026 are committed
//! at the batch epilogue, rate-limited.
//!
//! # Lifetime and privacy
//!
//! It holds only rows `text` already showed, lives in memory, is never part of
//! a checkpoint, is wiped by RIS, and is read through a clone-out API
//! ([`AltArchive::read`]) so the caller formats the reply after dropping the
//! terminal lock. `ATERM_ALT_ARCHIVE=0` (read once per process) turns it off by
//! default; [`Terminal::set_alt_archive_enabled`] and
//! [`Terminal::set_alt_archive_budget`] override per session.
//!
//! One thing outlives the process: a self-update HANDOFF carries it to the
//! process that adopts the session ([`AltArchiveCarry`]) — its origin and
//! indices, the differ's state, and the TAIL of its rows the host chose (the
//! GUI carries the rows after its recent turns' marks, at most 1 MiB, in a
//! `0600` file the adopting process deletes as it reads it), never shorter
//! than the rows the differ may still point back at. The capture is split
//! around the freeze: [`Terminal::alt_archive_carry_head`] takes the counters
//! (and, time allowing, the screen-sized state) under the lock the checkpoint
//! is taken under, and [`Terminal::alt_archive_carry_rows`] clones the rows
//! out afterwards — [`Terminal::alt_archive_carry_differ`] the state the
//! freeze left out — only while the [`AltArchiveFence`] still holds.
//! [`Terminal::alt_archive_import`] installs it and never panics on what it is
//! given. An archive that is off refuses it.

use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::{Arc, OnceLock};

use aterm_hash::{FxHashMap, FxHasher};

use super::Terminal;
use crate::grid::Grid;

/// Default retained-row accounting budget for one session's archive: 4 MiB.
/// Each retained row is charged by [`alt_archive_row_charge`]. This excludes
/// spare deque capacity, frame/differ buffers, gaps and other archive storage;
/// it is not a bound on the archive's total allocation or process RSS.
pub const ALT_ARCHIVE_DEFAULT_BUDGET: usize = 4 * 1024 * 1024;
/// Hard cap on archived rows regardless of the byte budget.
pub const ALT_ARCHIVE_MAX_ROWS: usize = 65_536;
/// Fixed charge per retained row: one deque element ([`ArchivedRow`]) plus
/// the two reference counts in its `Arc<str>` allocation.
///
/// The former flat 32-byte charge accounted for the deque element on a
/// 64-bit host but omitted the `Arc` header. Deriving these sizes keeps the
/// charge tied to the target's layout. Allocation overhead and unused deque
/// slots are outside this retained-row accounting policy.
pub const ALT_ARCHIVE_ROW_OVERHEAD: usize = size_of::<ArchivedRow>() + 2 * size_of::<usize>();

/// Retained-row charge for `len` text bytes: the fixed row overhead plus text
/// rounded up to a 16-byte boundary. The rounding is an accounting policy,
/// not a measurement or guarantee of the platform allocator's size classes.
///
/// Public so row admission, carry selection and callers use the same pricing.
#[must_use]
pub const fn alt_archive_row_charge(len: usize) -> usize {
    len.next_multiple_of(16) + ALT_ARCHIVE_ROW_OVERHEAD
}
/// Environment variable that turns the archive OFF by default for every terminal
/// this process creates: `ATERM_ALT_ARCHIVE=0` (also `off`, `false`, `no`).
pub const ALT_ARCHIVE_ENV: &str = "ATERM_ALT_ARCHIVE";

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
    /// `false`: the OLDEST `max_rows` rows after `since` (paging forward with
    /// `since=<last>`); `true`: the NEWEST `max_rows` rows (a tail).
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
/// that adopts the session: the counters, the gaps, the TAIL of the rows (a
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
        Self::charge_for(self.text.len())
    }

    /// [`Self::cost`] for a row whose text is `len` bytes, before it is built —
    /// the admission path prices a candidate with this.
    const fn charge_for(len: usize) -> usize {
        alt_archive_row_charge(len)
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

    fn derive(&mut self) {
        let n = self.text.len();
        self.hash.clear();
        self.anchor.clear();
        self.content.clear();
        self.hash.reserve(n);
        self.anchor.reserve(n);
        self.content.reserve(n);
        for s in &self.text {
            self.hash.push(row_hash(s));
            self.anchor.push(is_anchor_text(s));
            self.content.push(is_content_text(s));
        }
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

/// The pure differ + store. See the module docs.
pub struct AltArchive {
    rows: VecDeque<ArchivedRow>,
    bytes: usize,
    budget: usize,
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
    /// Fixed bottom chrome height: measured afresh on a shift, and only ever
    /// narrowed by the frames diffed after it (a status block replaced in place
    /// by an answer is no longer chrome). `None` until a frame was diffed on
    /// this baseline — a leave or restore flush then takes the whole screen, and
    /// a resize leaves out the rows the new frame still shows at the bottom
    /// ([`resize_region`](Self::resize_region)).
    chrome: Option<usize>,
    /// Rows that left the BOTTOM of the scrolling region when the content moved
    /// down (the app scrolled back), top first: displayed, never archived. They
    /// re-enter from the bottom when the app scrolls forward again; a flush
    /// (jump, resize, leave, restore) archives the ones that did not.
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
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte budget (0 = off).
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
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
    /// lost); a smaller budget evicts down to it.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        if budget == 0 {
            self.wipe();
            self.enabled = false;
        } else {
            self.enabled = true;
            self.evict();
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
    }

    // -------------------------------------------------------------- commits

    /// Commit one frame given as row strings (the pure entry point: tests and
    /// embedders). Each row is normalized exactly like extracted grid text.
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
    /// filled with NORMALIZED row text and then committed.
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

    /// Commit the frame in `cur` (filled through [`frame_buffers`]).
    fn commit_prepared(&mut self, cols: u16, key: Option<u64>) {
        let rows = self.cur.rows();
        if rows == 0 {
            return;
        }
        // A cleared screen is not a frame. Diffed, it reads as a jump whose flush
        // has no chrome to leave out (a clear before `?1049l` archived the
        // composer); installed, it is a baseline the next real frame jumps from
        // and reanchors into whatever came before. The frame before it stays the
        // baseline, so a leave or a redraw after the clear is judged against it.
        if self.cur.all_blank() {
            return;
        }
        self.cur.cols = cols;
        self.cur.derive();
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
        // Rule 2: identical text (a redundant ESU, a cursor-only or SGR-only frame).
        if self.prev.hash == self.cur.hash {
            self.prev_key = key;
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
    /// region once a diff measured the chrome. Until then the chrome is UNKNOWN —
    /// one installed frame, or an adopted screen the app only repainted unchanged
    /// (Rule 2 returns before anything is measured) — and flushing the whole
    /// screen archived the composer and footer. So the run of rows the new frame
    /// still shows at the bottom (aligned at the bottom, compared by
    /// [`Self::same_across_resize`]) is measured against it and left out. Counting
    /// too many is safe: a row that is really content is still on the new screen,
    /// and is archived when it scrolls off there.
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
        // Rows that did not move BELOW a block that did (a todo list and a
        // composer over a footer that ticks every frame, so rule 3 found no
        // chrome) outvote the few rows a big scroll leaves overlapping: they are
        // this frame's chrome, and the block above them moved.
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
        // row that says something vote, uniqueness still required. Only a MOVE
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
                // Rule 6: content moved UP by k.
                std::cmp::Ordering::Greater => {
                    self.chrome = Some(rows - region);
                    self.shift_up(vote.d.unsigned_abs(), vote.top);
                }
                // Rule 7: content moved DOWN (a block above shrank, or the user
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
    /// winner and the best NONZERO offset.
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

    /// Whether cur row `i` voted for offset `d` in the last (anchor) vote.
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
    /// stayed put ABOVE the block are a pinned header, which a shift handles).
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
    /// move). The leading run of rows equal at the same position is the most it
    /// can be; a row at the end of that run that ALSO matches the shifted
    /// position (a blank row, usually) is ambiguous, and the ambiguity resolves
    /// toward the pin the layout showed last time — a pinned blank read as moving
    /// would skip a real row, a moving blank read as pinned only displaces a
    /// blank.
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
        // Content growing up into an EMPTY top area (a blank screen filling from
        // the bottom): the rows that "left" are blank rows of an area that is
        // still there. Nothing scrolled off. (At the archive's start or after a
        // gap, `archive_picks` drops leading blanks anyway; this covers the rest.)
        if (s0..s0 + k).all(|i| self.prev.blank(i)) && self.cur.blank(s0) {
            return;
        }
        self.pin = s0;
        // Rows the screen was re-showing from the archive are skipped — each one
        // only if it IS the archived row it is assumed to be (a duplicate beats a
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

    /// Rule 7: the content moved DOWN by `j` over the region `[0, t)`. The rows
    /// revealed at the top are the archived rows just before the screen's old
    /// top (the debt). The rows pushed out at the bottom were displayed and are
    /// in no archive: every row below the block's lowest voter that did not land
    /// `j` rows further down waits in `below` until it re-enters or a flush
    /// archives it (a live zone row that merely changed lands there too — a
    /// duplicate or a stray status row beats a loss).
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
            // Older than what is retained (evicted, or left out of a handoff's
            // carry) or than this app run: unverifiable. Those rows — and the
            // re-shown ones with them — are archived again as they scroll off,
            // after a newest row they do not follow: a discontinuity, and a
            // reader is told (a duplicate beats a loss, a silent one does not).
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

    /// Rule 8's REANCHOR: where does the new screen's top sit in the recent
    /// archive? Returns the index of the archived row the screen shows at row
    /// `pin`, by a vote of the screen's anchors (`None` when it is nowhere: a
    /// duplicate can be cleaned, a loss cannot).
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

    /// Rule 9: keep the debt honest against the frame being committed — it is
    /// re-measured as the run of screen rows from `pin` that ARE consecutive
    /// archived rows from `debt_at`. A misaligned top ends it; a row that differs
    /// further down (new content entering below re-shown rows, an in-place edit)
    /// cuts it there, so the rows below get archived when they scroll off. Every
    /// row later SKIPPED on the debt's word is also verified individually in
    /// [`shift_up`](Self::shift_up).
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

    fn evict(&mut self) {
        while self.bytes > self.budget || self.rows.len() > self.max_rows {
            let Some(row) = self.rows.pop_front() else {
                break;
            };
            self.bytes -= row.cost();
            self.first += 1;
            self.lost += 1;
        }
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

    /// The app left the alternate screen. Flush the last committed frame's
    /// scrolling region (never the chrome below it) and the rows it pushed out
    /// at the bottom, record the gap, and drop the baseline. The caller commits
    /// the alt grid FIRST, while it still exists.
    pub fn leave(&mut self) {
        if !self.enabled {
            return;
        }
        if self.have_prev {
            let t = self.prev_region();
            self.flush_prev_rows(0, t);
        }
        self.push_gap(AltArchiveGapKind::Leave);
        self.epoch = self.epoch.wrapping_add(1);
        self.drop_prev();
        self.floor = self.last() + 1;
    }

    /// The app entered the alternate screen: a new baseline (no rows), and a
    /// new run — nothing archived before it is this screen's to re-show.
    pub fn enter(&mut self) {
        if !self.enabled {
            return;
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.drop_prev();
        self.floor = self.last() + 1;
    }

    /// The grids were replaced wholesale (checkpoint restore): flush, gap,
    /// new baseline.
    pub fn restore(&mut self) {
        if !self.enabled {
            return;
        }
        if self.have_prev {
            let t = self.prev_region();
            self.flush_prev_rows(0, t);
        }
        self.push_gap(AltArchiveGapKind::Restore);
        self.epoch = self.epoch.wrapping_add(1);
        self.drop_prev();
        self.floor = self.last() + 1;
    }

    /// Install the frame in the next-frame buffers (filled through
    /// [`fill_from_grid`](Self::fill_from_grid)) as the baseline WITHOUT diffing
    /// it: the screen a checkpoint restore just put up continues nothing this
    /// archive saw, but it is the screen the app's next frame changes. Diffed
    /// against it, that frame archives the rows it scrolled off (installed blind,
    /// they were lost), and a frame that only repaints it is Rule 2's no-op. A
    /// blank screen is no frame (see [`commit_prepared`](Self::commit_prepared)).
    fn install_restored(&mut self, cols: u16, key: Option<u64>) {
        if !self.enabled || self.cur.rows() == 0 || self.cur.all_blank() {
            return;
        }
        self.drop_prev();
        self.cur.cols = cols;
        self.cur.derive();
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
        self.gaps.clear();
        let last = self.last();
        if last > 0 {
            self.gaps.push_back(AltArchiveGap {
                after: last,
                kind: AltArchiveGapKind::Reset,
            });
        }
        self.epoch = self.epoch.wrapping_add(1);
        self.drop_prev();
        self.floor = self.last() + 1;
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

    /// The carry a handoff takes INSIDE its freeze: the counters, the gap at
    /// the newest row (a reader polling from there still needs it) and, with
    /// `differ`, the differ's screen-sized state. No row: every retained row
    /// counts in `lost` until [`carry_rows`](Self::carry_rows) attaches them
    /// off the frozen thread. On its own it is a valid carry, counters only:
    /// the indices go on, and a reader sees the rows it did not get as lost.
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

    /// Attach the differ's state to a carry whose head was taken without it
    /// (the freeze had no time to spare), off the frozen thread — only while
    /// the archive AND the differ still stand at `fence`: nothing was
    /// committed since, so the state is exactly the one the freeze would have
    /// taken beside its checkpoint, and the adopting differ goes on exactly
    /// ([`AltArchiveImport::Exact`]) instead of starting a new baseline after
    /// a `restore` gap. `false`, and `carry` untouched, when either moved or
    /// the carry already has one.
    pub fn carry_differ(&self, carry: &mut AltArchiveCarry, fence: AltArchiveFence) -> bool {
        if carry.differ.is_some() || self.fence() != fence || carry.origin != fence.origin {
            return false;
        }
        carry.differ = Some(self.differ_state());
        true
    }

    /// The oldest row the adopting differ may still point at, so a carry
    /// always reaches back to it: a reanchor searches the last
    /// [`REANCHOR_SCREENS`] screens of rows, and a scroll-back re-shows the
    /// rows just before the screen's top (the re-shown run from `debt_at`) —
    /// never rows before this app run's `floor`. A carry that left them out
    /// would make the adopting differ archive again rows this one recognizes
    /// as re-shown.
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
    /// [`carry_head`](Self::carry_head) — and never fewer than the differ's
    /// own reach (the last 8 screens of rows within this app run, and the run
    /// the screen shows again): the NEWEST of them, at most `max_bytes`
    /// charged like the budget (older ones stay counted in `lost`), with every
    /// gap a reader of them needs. Only while the archive's rows still stand
    /// at `fence` — `false`, and `carry` untouched, when they moved. One `Arc`
    /// clone per row.
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
            let cost = ArchivedRow::charge_for(text.len());
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

    /// Install a carry from the process this session was handed over from,
    /// in place of what this archive holds (the adopting side calls it right
    /// after restoring the checkpoint taken with it).
    ///
    /// It never panics and never leaves a state the differ could index out of
    /// range, whatever the carry says: indices past any an archive reaches
    /// refuse it; the rows are normalized and re-hashed; gaps are sorted into
    /// the carried range; and the differ's state is taken WHOLE or not at all
    /// — when any part of it does not fit the frame it describes or the rows
    /// it points at, none of it is, and the next frame starts a new baseline
    /// ([`AltArchiveImport::NoBaseline`]). This archive's own budget then
    /// evicts what it cannot hold. An archive that is off refuses the carry,
    /// so `ATERM_ALT_ARCHIVE=0` on the adopting side drops the carried rows.
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

/// Whether `ATERM_ALT_ARCHIVE` turns the archive off for this process (read
/// once).
fn env_opted_out() -> bool {
    static OPT_OUT: OnceLock<bool> = OnceLock::new();
    *OPT_OUT.get_or_init(|| {
        std::env::var(ALT_ARCHIVE_ENV).is_ok_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        })
    })
}

// =====================================================================
// The streaming DEC-mode matcher (A2): finds the `CSI ? Pm <final>` sequences
// that end a frame or switch screens, across read boundaries.
// =====================================================================

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
    /// On the alt screen: an ESU (`CSI ? … 2026 … l`) — split AFTER it and
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
    /// that may enter) — split AFTER it so the frames that follow in the same
    /// read are each committed.
    Enter,
    /// On the alt screen: RIS (`ESC c`), which leaves it — split AFTER it, so
    /// the wipe lands where it happened and the scan goes on for the MAIN
    /// screen (an enter right behind it is found).
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
/// without aborting it, CAN/SUB abort, ESC restarts, an intermediate or a
/// second private marker makes the sequence one it does not care about. Its
/// state survives across calls, so a sequence straddling two PTY reads is still
/// found.
///
/// Its cost rides on every byte the terminal receives, beside a parser that
/// scans them in bulk, so between sequences it never walks byte by byte. On
/// the alt screen it jumps from ESC to ESC. On the MAIN screen — where frames
/// commit nothing and only an alt-screen ENTER matters — it jumps from `?` to
/// `?` (plain text and SGR-heavy output carry none) and takes one only when
/// `ESC [` precedes it, the last two bytes of the previous read included. (A
/// C0 control between `ESC [` and `?`, which the grammar allows, is not seen
/// there: an enter spelled so is found at the next commit point instead of
/// splitting the read, and the parser never notices either way.)
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
                        // C0 controls execute and stay; ESC restarts; CAN/SUB abort.
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

// =====================================================================
// The Terminal hook (A2).
// =====================================================================

/// The archive plus the hook's bookkeeping — ONE `Terminal` field, session-only,
/// never forwarded to the handler, never checkpointed. A self-update handoff
/// carries the archive apart from the checkpoint ([`AltArchiveCarry`]); the
/// matcher and the commit bookkeeping start afresh in the adopting process.
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
    /// Enabled unless `ATERM_ALT_ARCHIVE=0` says otherwise.
    pub(super) fn new() -> Self {
        let mut archive = AltArchive::new();
        if env_opted_out() {
            archive.enabled = false;
        }
        Self {
            archive,
            matcher: DecModeMatcher::default(),
            seen_sync_seq: 0,
            seen_reset_gen: 0,
            on_alt: false,
            esu_seen: false,
            last_fallback: None,
        }
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

    /// Set the archive's byte budget; 0 turns it off and wipes it.
    pub fn set_alt_archive_budget(&mut self, budget: usize) {
        let was = self.alt_archive.archive.enabled;
        self.alt_archive.archive.set_budget(budget);
        if !was && self.alt_archive.archive.enabled {
            self.alt_archive_resync();
        }
    }

    /// Turn the archive on or off for this session (overrides
    /// `ATERM_ALT_ARCHIVE`). Off wipes it.
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

    /// Feed `input` through the parser, splitting after every ESU and one byte
    /// short of every alt-screen exit so the archive sees each committed frame
    /// and the alt grid before it is swapped away, and after an alt-screen enter
    /// so the frames behind it in the same read are split too. Slicing only
    /// moves where one `advance_fast` call ends and the next begins — the
    /// parser is a streaming state machine, so the grid is identical
    /// (`alt_archive_claude_like` proves it at 1..17-byte chunks).
    pub(super) fn advance_parser_archiving(&mut self, input: &[u8]) {
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

    /// Commit the active (alt) grid's rows to the archive.
    fn alt_archive_commit(&mut self) {
        let st = &mut self.alt_archive;
        if !st.archive.enabled {
            return;
        }
        let grid = &self.grid;
        let rows = grid.rows();
        let cols = grid.cols();
        if rows == 0 || cols == 0 {
            return;
        }
        let key = grid.content_gen();
        if st.archive.is_unchanged(key, usize::from(rows), cols) {
            return; // a redundant ESU does no work
        }
        st.archive.fill_from_grid(grid);
        st.archive.commit_prepared(cols, Some(key));
    }

    /// `restore_checkpoint` replaced the grids and modes wholesale: flush, gap,
    /// and — on the alternate screen — the restored screen is the new baseline.
    ///
    /// An adopted session (a self-update handoff) comes back showing the screen
    /// the old process had, and its app's next frame repaints or scrolls THAT
    /// screen. Diffed against it, the rows that frame scrolls off are archived
    /// rather than lost with a blind install. Not a screen captured inside an
    /// open 2026 window: that is a half-painted frame, and as a baseline it
    /// invites a false jump flush, or a chrome height measured on rows the app
    /// had not drawn yet (a later resize or leave would flush the composer).
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

    /// A self-update handoff's capture of this session's archive, taken
    /// INSIDE the freeze, under the lock its checkpoint is taken under: the
    /// fence and the counters-only carry, plus, with `differ`, the differ's
    /// screen-sized state. The rows are attached later, off the frozen thread
    /// ([`Terminal::alt_archive_carry_rows`]), and so is the differ's state
    /// when it was left out here ([`Terminal::alt_archive_carry_differ`]).
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

    /// Install a carried archive into an engine that has just restored the
    /// checkpoint taken with it ([`AltArchive::import`]). The origin, the
    /// indices, the rows and the differ's state go on from where the other
    /// process left them. When the differ's state did not come (or did not
    /// fit), the restored screen becomes the baseline after a `restore` gap
    /// — what a restore without a carry does.
    pub fn alt_archive_import(&mut self, carry: AltArchiveCarry) -> AltArchiveImport {
        let esu_seen = carry.differ.as_ref().is_some_and(|d| d.esu_seen);
        let outcome = self.alt_archive.archive.import(carry);
        match outcome {
            AltArchiveImport::Exact => self.alt_archive.esu_seen = esu_seen,
            AltArchiveImport::NoBaseline => {
                let archive = &mut self.alt_archive.archive;
                archive.push_gap(AltArchiveGapKind::Restore);
                archive.epoch = archive.epoch.wrapping_add(1);
                archive.floor = archive.last() + 1;
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
