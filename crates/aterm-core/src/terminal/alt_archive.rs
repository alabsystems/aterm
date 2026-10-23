// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Alternate-screen scroll-off archive ([`AltArchive`]): diffs committed frames by anchor voting and retains rows scrolled off the top.
use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use aterm_hash::{FxHashMap, FxHasher};

use super::Terminal;
use crate::grid::Grid;

/// Default retained-row byte budget for one archive ([`alt_archive_row_charge`]).
pub const ALT_ARCHIVE_DEFAULT_BUDGET: usize = 4 * 1024 * 1024;
/// Total retained-row byte budget shared across live sessions in one process ([`AltArchiveBudget::process`]).
pub const ALT_ARCHIVE_TOTAL_BUDGET: usize = ALT_ARCHIVE_DEFAULT_BUDGET;
/// Minimum byte budget floor for a session sharing [`ALT_ARCHIVE_TOTAL_BUDGET`].
pub const ALT_ARCHIVE_MIN_SHARE: usize = ALT_ARCHIVE_TOTAL_BUDGET / 16;
/// Maximum number of archived rows retained regardless of byte budget.
pub const ALT_ARCHIVE_MAX_ROWS: usize = 65_536;
/// Fixed byte overhead charged per retained row.
pub const ALT_ARCHIVE_ROW_OVERHEAD: usize = size_of::<ArchivedRow>() + 2 * size_of::<usize>();

/// Returns the retained-row byte charge for a row of `len` text bytes.
#[inline]
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

/// Cause of a discontinuity between archived rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AltArchiveGapKind {
    /// Redraw or scroll-back without reliable overlap.
    Jump,
    /// Screen resize flushed the previous screen.
    Resize,
    /// Application left the alternate screen.
    Leave,
    /// Full terminal reset wiped the archive.
    Reset,
    /// Checkpoint restore replaced the grids.
    Restore,
}

impl AltArchiveGapKind {
    /// Lowercase wire name (`jump`, `resize`, `leave`, `reset`, `restore`).
    #[inline]
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

    /// Parses a wire name into an [`AltArchiveGapKind`] (inverse of [`Self::as_str`]).
    #[inline]
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

/// Discontinuity between archived row `after` and row `after + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AltArchiveGap {
    /// Index of the last archived row before the discontinuity.
    pub after: u64,
    /// Cause of the discontinuity.
    pub kind: AltArchiveGapKind,
}

/// Query parameters for [`AltArchive::read`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltArchiveQuery {
    /// Exclusive lower row index bound (`0` reads from the beginning).
    pub since: u64,
    /// Maximum number of rows to return.
    pub max_rows: usize,
    /// `false` for oldest-first after `since`; `true` for newest `max_rows` rows.
    pub newest: bool,
}

impl AltArchiveQuery {
    /// Creates an oldest-first query for at most `max_rows` rows after `since`.
    #[inline]
    #[must_use]
    pub const fn oldest(since: u64, max_rows: usize) -> Self {
        Self {
            since,
            max_rows,
            newest: false,
        }
    }

    /// Creates a newest-first tail query for at most `max_rows` rows after `since`.
    #[inline]
    #[must_use]
    pub const fn newest(since: u64, max_rows: usize) -> Self {
        Self {
            since,
            max_rows,
            newest: true,
        }
    }
}

/// Snapshot of archived rows and metadata returned by [`AltArchive::read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltArchiveRead {
    /// Retained rows in oldest-first order.
    pub rows: Vec<Arc<str>>,
    /// Index of `rows[0]` (`last + 1` when `rows` is empty).
    pub first: u64,
    /// Newest archived row index (`0` if never archived; see [`Self::page_last`]).
    pub last: u64,
    /// Oldest retained row index (`last + 1` when empty).
    pub oldest: u64,
    /// Rows after `since` evicted or wiped before this read.
    pub lost: u64,
    /// Total rows ever evicted or wiped from this archive.
    pub lost_total: u64,
    /// Retained gaps with `after >= since`, oldest first.
    pub gaps: Vec<AltArchiveGap>,
    /// Number of rows at the top of the scrolling region already present in the archive.
    pub back: usize,
    /// Archived row index shown at screen row `pin` when `back > 0`.
    pub back_at: u64,
    /// Screen rows pinned above the scrolling region.
    pub pin: usize,
    /// Baseline generation counter.
    pub epoch: u32,
    /// Host-assigned origin identity for this archive.
    pub origin: u64,
    /// Whether additional matching rows were omitted by `max_rows`.
    pub more: bool,
    /// Whether the archive is recording.
    pub enabled: bool,
}

impl AltArchiveRead {
    /// Returns the index of the last row in this page (`last` when `rows` is empty).
    #[inline]
    #[must_use]
    pub const fn page_last(&self) -> u64 {
        match self.rows.len() {
            0 => self.last,
            n => self.first + n as u64 - 1,
        }
    }
}

/// Freeze-time state token used to verify consistency of a handoff carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltArchiveFence {
    /// Archive origin identity.
    pub origin: u64,
    /// Baseline generation counter.
    pub epoch: u32,
    /// Oldest retained row index.
    pub first: u64,
    /// Newest archived row index.
    pub last: u64,
    /// Total rows ever evicted or wiped.
    pub lost: u64,
    /// Number of retained gaps.
    pub gaps: usize,
    /// Fingerprint of the differ state.
    pub differ: u64,
}

impl AltArchiveFence {
    /// The fence without the differ's fingerprint: what the retained rows
    /// depend on.
    #[inline]
    const fn rows_part(self) -> Self {
        Self { differ: 0, ..self }
    }
}

/// Exported differ state carried across a self-update handoff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AltArchiveDiffer {
    /// Width and normalized row strings of the last committed frame.
    pub prev: Option<(u16, Vec<String>)>,
    /// Measured bottom chrome height.
    pub chrome: Option<usize>,
    /// Rows pinned above the scrolling region.
    pub pin: usize,
    /// Count of archived rows re-shown from screen row `pin`.
    pub debt: usize,
    /// Archived row index at screen row `pin` when `debt > 0`.
    pub debt_at: u64,
    /// Rows pushed below the bottom of the scrolling region.
    pub below: Vec<String>,
    /// Whether DEC 2026 synchronized updates were seen in this alt-screen epoch.
    pub esu_seen: bool,
}

/// Serialized archive state carried across a self-update handoff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AltArchiveCarry {
    /// Archive origin identity.
    pub origin: u64,
    /// Index of `rows[0]` (`last + 1` when `rows` is empty).
    pub first: u64,
    /// Rows evicted, wiped, or omitted from this carry.
    pub lost: u64,
    /// Minimum row index eligible for reanchoring or scroll-back.
    pub floor: u64,
    /// Baseline generation counter.
    pub epoch: u32,
    /// Whether the archive was recording.
    pub enabled: bool,
    /// Carried gaps, oldest first.
    pub gaps: Vec<AltArchiveGap>,
    /// Carried rows, oldest first.
    pub rows: Vec<Arc<str>>,
    /// Optional carried differ state.
    pub differ: Option<AltArchiveDiffer>,
}

impl AltArchiveCarry {
    /// Returns the newest row index represented by this carry.
    #[inline]
    #[must_use]
    pub const fn last(&self) -> u64 {
        (self.first + self.rows.len() as u64).saturating_sub(1)
    }
}

/// Outcome of [`AltArchive::import`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltArchiveImport {
    /// Counters, gaps, rows, and differ baseline were all installed.
    Exact,
    /// Counters, gaps, and rows were installed without a differ baseline.
    NoBaseline,
    /// Carry was rejected because the archive is disabled or indices are out of range.
    Refused,
}

/// No archive reaches an index this high (one row per commit); an imported index past it
/// would overflow later arithmetic.
const MAX_CARRIED_INDEX: u64 = 1 << 62;

#[derive(Debug, Clone)]
struct ArchivedRow {
    text: Arc<str>,
    hash: u64,
    anchor: bool,
}

impl ArchivedRow {
    /// This row's charge against the retained-row budget.
    #[inline]
    fn cost(&self) -> usize {
        alt_archive_row_charge(self.text.len())
    }
}

/// One committed frame: trimmed row text with its hash and anchor eligibility.
#[derive(Debug, Default)]
struct Frame {
    text: Vec<String>,
    hash: Vec<u64>,
    anchor: Vec<bool>,
    /// A row that says something (not blank, not only box/rule art), however short: the
    /// eligibility of the weak vote and the sparse test.
    content: Vec<bool>,
    cols: u16,
}

impl Frame {
    #[inline]
    const fn rows(&self) -> usize {
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

    #[inline]
    fn blank(&self, i: usize) -> bool {
        self.text[i].is_empty()
    }

    /// Whether every row is blank (a cleared screen).
    #[inline]
    fn all_blank(&self) -> bool {
        self.text.iter().all(String::is_empty)
    }
}

/// Reused per-commit scratch (maps keep their capacity across frames).
#[derive(Debug, Default)]
struct Scratch {
    prev_idx: FxHashMap<u64, (u32, usize)>,
    cur_idx: FxHashMap<u64, (u32, usize)>,
    /// Indexed by `d + T - 1`: (votes, cur positions of the topmost and lowest voter).
    votes: Vec<(u32, usize, usize)>,
    tail_idx: FxHashMap<u64, usize>,
    j_votes: FxHashMap<usize, u32>,
    picks: Vec<usize>,
}

/// Shared retained-row byte budget pool across live archives.
#[derive(Debug)]
pub struct AltArchiveBudget {
    total: usize,
    min_share: usize,
    live: AtomicUsize,
}

impl AltArchiveBudget {
    /// Creates a budget pool of `total` bytes with per-archive floor `min_share`.
    #[inline]
    #[must_use]
    pub fn new(total: usize, min_share: usize) -> Arc<Self> {
        Arc::new(Self {
            total,
            min_share: min_share.min(total),
            live: AtomicUsize::new(0),
        })
    }

    /// Returns the process-wide shared budget pool ([`ALT_ARCHIVE_TOTAL_BUDGET`], [`Terminal::set_alt_archive_shared`]).
    #[must_use]
    pub fn process() -> Arc<Self> {
        static POOL: OnceLock<Arc<AltArchiveBudget>> = OnceLock::new();
        Arc::clone(
            POOL.get_or_init(|| {
                AltArchiveBudget::new(ALT_ARCHIVE_TOTAL_BUDGET, ALT_ARCHIVE_MIN_SHARE)
            }),
        )
    }

    /// Returns the total bytes in this pool.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the number of active archives currently drawing on this pool.
    #[inline]
    #[must_use]
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// Returns the byte budget currently allotted to a single active archive.
    #[inline]
    #[must_use]
    pub fn share(&self) -> usize {
        (self.total / self.live().max(1)).max(self.min_share)
    }
}

/// One archive's membership of a pool; `counted` keeps the live count's increment and
/// decrement paired, including on drop (a closed tab gives its share back).
#[derive(Debug)]
struct BudgetShare {
    pool: Arc<AltArchiveBudget>,
    counted: bool,
}

impl BudgetShare {
    #[inline]
    const fn new(pool: Arc<AltArchiveBudget>) -> Self {
        Self {
            pool,
            counted: false,
        }
    }

    #[inline]
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

/// Alternate-screen scroll-off differ and row store.
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
    /// Rows at the top of the scrolling region (screen rows `[pin, pin+debt)`) that are already
    /// archived as rows `[debt_at, debt_at+debt)`: usually the archive's last rows (the app
    /// scrolled back), mid-archive after a reanchor (a view toggled away and back).
    debt: usize,
    /// Absolute index of the archived row shown at screen row `pin` (meaningful
    /// while `debt > 0`).
    debt_at: u64,
    /// Rows pinned above the scrolling region in the last shift.
    pin: usize,
    /// Fixed bottom chrome height: measured on a shift, then only narrowed on this baseline.
    chrome: Option<usize>,
    /// Rows that left the bottom of the scrolling region when the content moved down, top
    /// first: displayed, not archived.
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
    /// Creates an enabled archive with [`ALT_ARCHIVE_DEFAULT_BUDGET`].
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

    /// Creates an archive with an explicit byte budget and row limit.
    #[must_use]
    #[cfg(test)]
    pub fn with_limits(budget: usize, max_rows: usize) -> Self {
        let mut a = Self::new();
        a.budget = budget;
        a.max_rows = max_rows.max(1);
        a.enabled = budget > 0;
        a
    }

    /// Returns the newest archived row index (`0` if never archived).
    #[inline]
    #[must_use]
    pub fn last(&self) -> u64 {
        self.first + self.rows.len() as u64 - 1
    }

    /// Returns the oldest retained row index (`last + 1` when empty).
    #[inline]
    #[must_use]
    pub const fn oldest(&self) -> u64 {
        self.first
    }

    /// Returns the number of retained rows.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns whether no rows are currently retained.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns the current byte charge of retained rows.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Returns this archive's configured byte budget before pool sharing.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn budget(&self) -> usize {
        self.budget
    }

    /// Returns the effective byte budget after applying any shared pool cap ([`Self::share_budget`]).
    #[inline]
    #[must_use]
    pub fn effective_budget(&self) -> usize {
        match &self.share {
            Some(s) => self.budget.min(s.pool.share()),
            None => self.budget,
        }
    }

    /// Returns the shared budget pool this archive draws on, if any.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn shared_budget(&self) -> Option<&Arc<AltArchiveBudget>> {
        self.share.as_ref().map(|s| &s.pool)
    }

    /// Returns the total number of rows ever evicted or wiped.
    #[inline]
    #[must_use]
    pub const fn lost(&self) -> u64 {
        self.lost
    }

    /// Returns the baseline generation counter (see [`AltArchiveRead::epoch`]).
    #[inline]
    #[must_use]
    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Returns the host-assigned origin identity.
    #[inline]
    #[must_use]
    pub const fn origin(&self) -> u64 {
        self.origin
    }

    /// Returns the number of archived rows currently re-shown at the top of the screen.
    #[inline]
    #[must_use]
    pub const fn back(&self) -> usize {
        self.debt
    }

    /// Returns the archived row index at the top of the scrolling region when [`Self::back`] is nonzero.
    #[inline]
    #[must_use]
    pub const fn back_at(&self) -> u64 {
        if self.debt > 0 { self.debt_at } else { 0 }
    }

    /// Returns whether the archive is recording.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns an iterator over retained gaps, oldest first.
    #[inline]
    pub fn gaps(&self) -> impl Iterator<Item = AltArchiveGap> + '_ {
        self.gaps.iter().copied()
    }

    /// Returns the text of retained row `index`, if present.
    #[inline]
    #[must_use]
    pub fn row(&self, index: u64) -> Option<&str> {
        let off = usize::try_from(index.checked_sub(self.first)?).ok()?;
        self.rows.get(off).map(|r| &*r.text)
    }

    /// Returns all retained row strings, oldest first.
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.text.to_string()).collect()
    }

    /// Clones a page of rows matching `q`.
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

    /// Sets the host-assigned origin identity.
    #[inline]
    pub const fn set_origin(&mut self, origin: u64) {
        self.origin = origin;
    }

    /// Sets the byte budget (`0` disables and wipes the archive) and evicts excess rows.
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

    /// Attaches or detaches a shared [`AltArchiveBudget`] pool and evicts to fit.
    pub fn share_budget(&mut self, pool: Option<Arc<AltArchiveBudget>>) {
        let same = match (&self.share, &pool) {
            (Some(s), Some(p)) => Arc::ptr_eq(&s.pool, p),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.share = None;
        self.share = pool.map(BudgetShare::new);
        self.sync_share();
        self.evict();
    }

    /// Count this archive among its pool's live archives exactly while it is enabled and holds
    /// a row; called wherever `bytes` can cross zero.
    fn sync_share(&mut self) {
        let drawing = self.enabled && self.bytes > 0;
        if let Some(share) = &mut self.share {
            share.set_counted(drawing);
        }
    }

    /// Enables or disables recording (disabling wipes retained rows).
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

    /// Commits a frame provided as row strings.
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

    /// Fill the next frame's row buffers with `grid`'s screen rows, normalized as `text` shows.
    fn fill_from_grid(&mut self, grid: &Grid) {
        let rows = grid.rows();
        let bufs = self.frame_buffers(usize::from(rows));
        for (r, buf) in (0..rows).zip(bufs.iter_mut()) {
            grid.row_text_screen_into(r, buf);
            normalize_row(buf);
        }
    }

    /// Commit `grid`'s screen rows (the active alt grid) as the next frame, keyed by its
    /// content generation so a redundant ESU does no work.
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

    /// Commit the frame in `cur` (filled through [`frame_buffers`](Self::frame_buffers)).
    fn commit_prepared(&mut self, cols: u16, key: Option<u64>) {
        self.evict();
        let rows = self.cur.rows();
        if rows == 0 {
            return;
        }
        if self.cur.all_blank() {
            return;
        }
        self.cur.cols = cols;
        self.cur.derive();
        if self.have_prev
            && self.prev.cols == cols
            && self.prev.rows() == rows
            && self.prev.hash == self.cur.hash
        {
            self.prev_key = key;
            return;
        }
        self.cur.derive_votes();
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
        let mut b = 0;
        while b < rows && self.prev.hash[rows - 1 - b] == self.cur.hash[rows - 1 - b] {
            b += 1;
        }
        let t = rows - b;
        self.diff(t, b);
        self.check_debt(t);
        self.install_cur(key);
    }

    const fn install_cur(&mut self, key: Option<u64>) {
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

    /// What a resize flushes of the last frame (Rule 1): its scrolling region once a diff
    /// measured the chrome; until then, everything above the bottom run the new frame still
    /// shows ([`Self::same_across_resize`]).
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
        if best.d == 0
            && best.votes >= need
            && let Some(floor) = self.fixed_floor(t, moved)
            && moved.votes >= self.need(floor, false)
        {
            need = self.need(floor, false);
            (vote, region) = (moved, floor);
        }
        if vote.votes < need {
            let (weak, _) = self.vote(t, true);
            let weak_need = self.need(t, true);
            if weak.d != 0 && weak.votes >= weak_need {
                (vote, need) = (weak, weak_need);
            }
        }
        if vote.votes >= need {
            match vote.d.cmp(&0) {
                std::cmp::Ordering::Equal => self.narrow_chrome(b),
                std::cmp::Ordering::Greater => {
                    self.chrome = Some(rows - region);
                    self.shift_up(vote.d.unsigned_abs(), vote.top);
                }
                std::cmp::Ordering::Less => {
                    self.chrome = Some(rows - region);
                    self.shift_down(vote, region);
                }
            }
            self.settle_below();
            return;
        }
        self.narrow_chrome(b);
        let changed = (0..t)
            .filter(|&i| self.prev.hash[i] != self.cur.hash[i])
            .count();
        if changed <= (t / 4).max(2) {
            self.settle_below();
            return;
        }
        let says = |f: &Frame| f.content[..t].iter().filter(|&&c| c).count();
        if says(&self.prev) < NEED_VOTES && says(&self.cur) < NEED_VOTES {
            self.settle_below();
            return;
        }
        self.jump(t);
    }

    /// Anchor voting (rule 4) over `[0, t)`: eligible rows (anchors, or with `weak` any row
    /// with content) unique in both frames vote for the offset `d = pos_prev - pos_cur`.
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
                // a hash collision never votes
                continue;
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

    /// Rows pinned above a block that moved up by `k`: at most the leading run equal at the
    /// same position; a trailing row that also matches the shifted position (usually blank)
    /// resolves toward last time's pin.
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
        if (s0..s0 + k).all(|i| self.prev.blank(i)) && self.cur.blank(s0) {
            return;
        }
        self.pin = s0;
        let mut r = 0;
        while r < k.min(self.debt)
            && self.archived_hash(self.debt_at + r as u64) == Some(self.prev.hash[s0 + r])
        {
            r += 1;
        }
        if r == k && self.debt >= k {
            self.debt_at += k as u64;
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

    /// Rule 7: the content moved down by `j` over `[0, t)`. Rows revealed at the top are the
    /// archived rows just before the old top (the debt); rows pushed out at the bottom wait in
    /// `below` until they re-enter or a flush archives them.
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
        self.below.truncate(REANCHOR_SCREENS * self.cur.rows());
        self.pin = self.pin.min(self.leading_equal(top_prev));
        let base = if self.debt > 0 {
            self.debt_at
        } else {
            self.last() + 1
        };
        if base >= self.first.max(self.floor) + j as u64 {
            self.debt_at = base - j as u64;
            self.debt += j;
        } else {
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
        let q = self.leading_equal(t);
        self.flush_prev_rows(q, t);
        self.push_gap(AltArchiveGapKind::Jump);
        self.debt = 0;
        if let Some(at) = self.reanchor(t) {
            self.debt_at = at;
            self.debt = 1;
        }
    }

    /// Rule 8's reanchor: the archived row the new screen shows at row `pin`, by a vote of the
    /// screen's anchors; `None` when it is nowhere in the recent archive.
    fn reanchor(&mut self, t: usize) -> Option<u64> {
        let len = self.rows.len();
        if len == 0 || t <= self.pin {
            return None;
        }
        let floor = usize::try_from(self.floor.saturating_sub(self.first)).unwrap_or(usize::MAX);
        let lo = len
            .saturating_sub(REANCHOR_SCREENS * self.cur.rows())
            .max(floor);
        let s = &mut self.scratch;
        s.tail_idx.clear();
        for pos in lo..len {
            let row = &self.rows[pos];
            if row.anchor {
                // the most recent occurrence wins
                s.tail_idx.insert(row.hash, pos);
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

    /// Rule 9: re-measure the debt as the run of screen rows from `pin` that are consecutive
    /// archived rows from `debt_at`; the first differing row cuts it.
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

    /// Archive the rows of `prev` in `[from, to)` outside the debt region (already archived),
    /// then the rows waiting in `below`, trimming blank edges (a gap always follows a flush).
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

    /// Append `prev[i]` for every `i` in `scratch.picks` (rows a shift scrolled off), dropping
    /// leading blanks that would open the archive or follow a gap.
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

    /// Evict the oldest rows until inside the own budget and pool share; the share falls as
    /// sessions open, so an archive can be over a budget it never set.
    fn evict(&mut self) {
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
        self.sync_share();
        while self.gaps.front().is_some_and(|g| g.after + 1 < self.first) {
            self.gaps.pop_front();
        }
        if self.debt > 0 && self.debt_at < self.first {
            // the re-shown rows were evicted: nothing left to skip
            self.debt = 0;
        }
    }

    /// Flushes the scrolling region on alternate-screen exit and records a `Leave` gap.
    #[inline]
    pub fn leave(&mut self) {
        self.end_run(false, AltArchiveGapKind::Leave);
    }

    /// Flushes the entire screen on orphaned alternate-screen exit and records a `Leave` gap.
    #[inline]
    pub fn leave_whole(&mut self) {
        self.end_run(true, AltArchiveGapKind::Leave);
    }

    /// Starts a new baseline run on alternate-screen entry.
    #[inline]
    pub fn enter(&mut self) {
        if self.enabled {
            self.new_run();
        }
    }

    /// Flushes and starts a new baseline after a checkpoint restore.
    #[inline]
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

    /// Install the frame in the next-frame buffers (filled by
    /// [`fill_from_grid`](Self::fill_from_grid)) as the baseline without diffing it: a restored
    /// screen continues nothing this archive saw but is what the app's next frame changes.
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

    /// Discards all retained rows, records a `Reset` gap, and drops the baseline.
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

    /// Returns the current [`AltArchiveFence`] state token.
    #[inline]
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

    /// Captures the freeze-time fence and header of an [`AltArchiveCarry`].
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

    /// A fingerprint of exactly what [`differ_state`](Self::differ_state) takes, from hashes
    /// the differ already keeps: no allocation, cheap enough for inside the freeze.
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

    /// Attaches differ state to `carry` if the archive still matches `fence`.
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

    /// Attaches retained rows within `budget` bytes to `carry` if the archive still matches `fence`.
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

    /// Imports `carry` from a handoff, replacing current archive state.
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
/// `below` normalized and within its bound.
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
    #[inline]
    const fn beats(self, other: Self) -> bool {
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

#[inline]
fn row_hash(s: &str) -> u64 {
    let mut h = FxHasher::default();
    h.write(s.as_bytes());
    h.write_usize(s.len());
    h.finish()
}

/// Box-drawing / rule chars (and space): a row made only of these never votes.
#[inline]
const fn is_rule_char(ch: char) -> bool {
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
#[inline]
fn said_text(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().filter(|&ch| !is_rule_char(ch))
}

/// A row that says something: not blank, not only rules/box art.
#[inline]
fn is_content_text(s: &str) -> bool {
    s.chars().any(|ch| !is_rule_char(ch))
}

/// A row distinctive enough to vote: at least [`MIN_ANCHOR_CHARS`] visible chars
/// after trimming, not only rules/box art.
#[inline]
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

/// Normalize extracted row text to exactly what `text` shows: NUL/control chars become a
/// space (aterm-control's `visible_char`, so the hash and archived bytes match the screen
/// reply), then trailing whitespace is trimmed.
fn normalize_row(s: &mut String) {
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

/// The first index of `needle` in `hay`. The matcher sees every byte the terminal receives, so
/// it compares 64-byte blocks (which the compiler vectorizes) and walks bytes only inside the
/// block that holds a hit.
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

/// Whether `input` holds conhost's alt-screen enter, `ESC [ ? 1049 h`. It runs over every read
/// while `TransientState::conhost_alt_screen_left_up` is set (for a cmd.exe session, possibly
/// the rest of the session), so it hops ESC to ESC with [`find_byte`].
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
    /// On the alt screen: XTRESTORE of alt-screen modes (`CSI ? 1049 r`; the payload is the
    /// `ALT_*` bits it names), which exits when the saved value is "off": split one byte short
    /// and commit first if it will.
    Restore(u8),
    /// On the main screen: an alt-screen enter (`CSI ? 1049 h`, or an XTRESTORE that may
    /// enter): split after it so each following frame in the read is committed.
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

/// Streaming recognizer for DEC private mode sets, resets and XTRESTOREs. It mirrors the
/// VT500 CSI grammar closely enough that its split points are always at a final byte: C0
/// controls inside a CSI execute without aborting it, CAN/SUB abort, ESC restarts, an
/// intermediate or a second private marker makes the sequence irrelevant.
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
    #[inline]
    const fn finish_param(&mut self) {
        match self.param {
            2026 => self.esu = true,
            47 => self.alt |= ALT_47,
            1047 => self.alt |= ALT_1047,
            1049 => self.alt |= ALT_1049,
            _ => {}
        }
        self.param = 0;
    }

    #[inline]
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// What a completed `CSI ? … <final>` means for the hook, given the screen.
    #[inline]
    const fn classify(&self, final_byte: u8, on_alt: bool) -> Option<HitKind> {
        match (final_byte, on_alt) {
            (b'l', true) if self.alt != 0 => Some(HitKind::Exit),
            (b'l', true) if self.esu => Some(HitKind::Esu),
            (b'r', true) if self.alt != 0 => Some(HitKind::Restore(self.alt)),
            (b'h' | b'r', false) if self.alt != 0 => Some(HitKind::Enter),
            _ => None,
        }
    }

    /// Advance over `bytes`, returning at the first sequence the hook acts on (its final byte
    /// consumed), or `None` with the state carried to the next call.
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

/// Per-session alternate-screen archive state and parser hook bookkeeping.
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
    /// Creates an enabled archive state.
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

    /// Flushes the alternate screen when left without an application exit sequence.
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
    /// Returns a shared reference to the [`AltArchive`].
    #[inline]
    #[must_use]
    pub const fn alt_archive(&self) -> &AltArchive {
        &self.alt_archive.archive
    }

    /// Reads a page from the alternate-screen archive (see [`AltArchive::read`]).
    #[inline]
    #[must_use]
    pub fn alt_archive_read(&self, q: AltArchiveQuery) -> AltArchiveRead {
        self.alt_archive.archive.read(q)
    }

    /// Sets the archive's origin identity.
    #[inline]
    pub const fn set_alt_archive_origin(&mut self, origin: u64) {
        self.alt_archive.archive.set_origin(origin);
    }

    /// Joins or leaves the process-wide archive budget pool ([`AltArchiveBudget::process`]).
    #[inline]
    pub fn set_alt_archive_shared(&mut self, shared: bool) {
        self.alt_archive
            .archive
            .share_budget(shared.then(AltArchiveBudget::process));
    }

    /// Sets this session's archive byte budget (`0` disables and wipes it).
    #[inline]
    pub fn set_alt_archive_budget(&mut self, budget: usize) {
        let was = self.alt_archive.archive.enabled;
        self.alt_archive.archive.set_budget(budget);
        if !was && self.alt_archive.archive.enabled {
            self.alt_archive_resync();
        }
    }

    /// Enables or disables the alternate-screen archive for this session.
    #[inline]
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

    /// Advances the VT parser over `input`, committing archive frames at mode boundaries.
    pub(super) fn advance_parser_archiving(&mut self, input: &[u8]) {
        // A host-forced primary screen runs before the batch's bytes, like an in-band `?1049l`.
        if std::mem::take(&mut self.transient.host_leave_alternate_screen) {
            let (_, mut handler) = self.split_for_process();
            handler.leave_orphaned_alternate_screen();
        }
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

    #[inline]
    fn advance_parser_raw(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let (parser, mut handler) = self.split_for_process();
        parser.advance_fast(bytes, &mut handler);
    }

    /// Whether an XTRESTORE naming these alt-screen modes (`ALT_*` bits) turns the alternate
    /// screen off: one of them was saved as "off".
    fn xtrestore_leaves_alt(&self, modes: u8) -> bool {
        [(ALT_47, 47u16), (ALT_1047, 1047), (ALT_1049, 1049)]
            .iter()
            .any(|&(bit, mode)| {
                modes & bit != 0 && self.transient.xtsave_modes.get(&mode) == Some(&false)
            })
    }

    /// A commit point: a reset since the last one wipes; a screen switch leaves or enters; a
    /// moved `sync_end_seq` (ESU, sync timeout, DECSTR) on the alt screen commits the frame.
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

    /// Commits pending synchronized updates or fallback frames at the end of `process_at`.
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

    /// Commit the active (alt) grid's rows; callers run only while the archive is enabled.
    #[inline]
    fn alt_archive_commit(&mut self) {
        self.alt_archive.archive.commit_grid(&self.grid);
    }

    /// Resets the archive baseline after `restore_checkpoint`.
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

    /// On the alternate screen and outside an open 2026 window: install the screen as the
    /// archive's baseline without diffing it.
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

    /// Captures the freeze-time fence and carry header for this session's archive.
    #[must_use]
    pub fn alt_archive_carry_head(&self, differ: bool) -> (AltArchiveFence, AltArchiveCarry) {
        let (fence, mut carry) = self.alt_archive.archive.carry_head(differ);
        if let Some(d) = carry.differ.as_mut() {
            d.esu_seen = self.alt_archive.esu_seen;
        }
        (fence, carry)
    }

    /// Attaches differ state to `carry` if `fence` still holds (see [`AltArchive::carry_differ`]).
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

    /// Attaches retained rows to `carry` if `fence` still holds (see [`AltArchive::carry_rows`]).
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

    /// Imports a handoff `carry` after checkpoint restore ([`AltArchive::import`]).
    pub fn alt_archive_import(&mut self, carry: AltArchiveCarry) -> AltArchiveImport {
        let esu_seen = carry.differ.as_ref().is_some_and(|d| d.esu_seen);
        let outcome = self.alt_archive.archive.import(carry);
        match outcome {
            AltArchiveImport::Exact => self.alt_archive.esu_seen = esu_seen,
            AltArchiveImport::NoBaseline => {
                self.alt_archive.archive.restore();
                self.alt_archive.esu_seen = false;
                self.alt_archive_install_screen();
            }
            AltArchiveImport::Refused => {}
        }
        outcome
    }

    /// Wipes the archive and resets the mode matcher on a direct host reset.
    pub(super) fn alt_archive_after_host_reset(&mut self) {
        self.alt_archive.matcher.reset();
        self.alt_archive_boundary();
    }
}

#[cfg(test)]
#[path = "alt_archive_tests.rs"]
mod tests;
