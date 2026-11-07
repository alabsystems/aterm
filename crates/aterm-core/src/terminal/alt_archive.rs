// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Alternate-screen scroll-off archive ([`AltArchive`]): diffs committed frames by anchor voting and retains rows scrolled off the top.
use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use aterm_hash::FxHasher;

use super::Terminal;
use crate::grid::Grid;
use crate::grid::cell::{Cell, CellFlags, PackedColors};

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
const MIN_ANCHOR_CHARS: usize = 4;
const NEED_VOTES: usize = 3;
const REANCHOR_SCREENS: usize = 8;
/// The most rows a frame may change for a commit to take it in place
/// ([`AltArchive::commit_in_place`]): each is checked against every row of the previous frame.
const IN_PLACE_ROWS: usize = 8;
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

const MAX_CARRIED_INDEX: u64 = 1 << 62;

/// A retained row: where its text sits in the archive's [`TextStore`], with its hash and anchor
/// eligibility.
#[derive(Debug, Clone, Copy)]
struct ArchivedRow {
    /// The number of the chunk holding its text ([`TextStore::first`]).
    chunk: u64,
    /// Where its text starts in the chunk.
    start: u32,
    /// Its text's length in bytes.
    len: u32,
    hash: u64,
    anchor: bool,
}

impl ArchivedRow {
    /// This row's charge against the retained-row budget.
    #[inline]
    fn cost(&self) -> usize {
        alt_archive_row_charge(self.len as usize)
    }
}

/// The retained rows' text, end to end in chunks: a row's text is a span of one chunk, so
/// archiving a row copies its text without an allocation of its own and a wipe frees a few
/// chunks rather than every row. A chunk is dropped once no retained row is in it; the last one
/// dropped is kept for the next chunk to fill, so steady eviction allocates nothing.
#[derive(Debug, Default)]
struct TextStore {
    /// The full chunks in the order they were filled.
    chunks: VecDeque<String>,
    /// The chunk taking the next text, numbered after the full ones. A text goes in only
    /// within its capacity, so no text it holds moves.
    open: String,
    /// The number of `chunks[0]` (of `open` when there is none). Numbers count every chunk
    /// since the store began, so a row's chunk number stays its own while the chunks before it
    /// go.
    first: u64,
    /// The last chunk dropped, emptied, to become the next one.
    spare: String,
}

impl TextStore {
    /// The room a new chunk takes (a longer text gets a chunk of its own).
    const CHUNK: usize = 16 * 1024;

    /// Stores `text` and returns the retained row it is, with `hash` and `anchor`. A text longer
    /// than `u32::MAX` bytes (no row is) is stored empty.
    #[inline]
    fn push(&mut self, text: &str, hash: u64, anchor: bool) -> ArchivedRow {
        let Ok(len) = u32::try_from(text.len()) else {
            return self.push("", hash, anchor);
        };
        if self.open.capacity() - self.open.len() < text.len() {
            self.open_room(text.len());
        }
        let start = self.open.len();
        self.open.push_str(text);
        ArchivedRow {
            chunk: self.first + self.chunks.len() as u64,
            // Only a chunk past `u32::MAX` bytes, which holds one text, starts one past it.
            start: u32::try_from(start).unwrap_or(0),
            len,
            hash,
            anchor,
        }
    }

    /// Makes room for a text of `len` bytes in the open chunk: one that has some text is full,
    /// and the spare takes its place.
    #[cold]
    fn open_room(&mut self, len: usize) {
        if !self.open.is_empty() {
            let next = std::mem::take(&mut self.spare);
            self.chunks
                .push_back(std::mem::replace(&mut self.open, next));
        }
        self.open.reserve(len.max(Self::CHUNK));
    }

    /// The text of `row`, which must be in a chunk not yet dropped.
    #[inline]
    fn text(&self, row: &ArchivedRow) -> &str {
        let at = usize::try_from(row.chunk.wrapping_sub(self.first)).unwrap_or(usize::MAX);
        let (start, len) = (row.start as usize, row.len as usize);
        let chunk = if at == self.chunks.len() {
            Some(&self.open)
        } else {
            self.chunks.get(at)
        };
        let text = chunk.and_then(|c| c.get(start..start + len));
        debug_assert!(text.is_some(), "a retained row's text is in a live chunk");
        text.unwrap_or_default()
    }

    /// Drops the chunks numbered below `keep`: those no retained row is in when `keep` is the
    /// oldest row's chunk, or every chunk at `u64::MAX`. The open chunk is emptied in place,
    /// keeping its number.
    fn release_before(&mut self, keep: u64) {
        while self.first < keep {
            let Some(mut chunk) = self.chunks.pop_front() else {
                self.open.clear();
                break;
            };
            self.first += 1;
            if chunk.capacity() <= 2 * Self::CHUNK {
                chunk.clear();
                self.spare = chunk;
            }
        }
    }

    /// The bytes of text the chunks hold.
    #[cfg(test)]
    fn held(&self) -> usize {
        self.chunks.iter().map(String::len).sum::<usize>() + self.open.len()
    }

    /// Drops every chunk and the spare: the store holds no memory.
    fn clear(&mut self) {
        self.first += self.chunks.len() as u64;
        self.chunks = VecDeque::new();
        self.open = String::new();
        self.spare = String::new();
    }
}

/// A row that left the bottom of the scrolling region, holding its own text until a flush
/// archives it or the screen shows it again ([`AltArchive::below`]).
#[derive(Debug, Clone)]
struct HeldRow {
    text: String,
    hash: u64,
    anchor: bool,
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
    /// Where each hash's eligible rows sit ([`Places`]), built once for the frame: a vote reads
    /// it as the current frame and the next commit's vote as the previous one.
    index: HashIndex<Places>,
    /// Each row's [`Places`] in `index`, built with it ([`Places::EMPTY`] for a row eligible in
    /// neither class), so a vote reads a row's own without a lookup.
    places: Vec<Places>,
    /// [`Self::index`]'s scratch: each row's slot in `index`, or [`NO_ROW`].
    slots: Vec<usize>,
    /// Each row's version ([`MemoRow::version`]), 0 for a row whose text came from elsewhere.
    /// Two frames' rows with one version have one text: that of a memo entry no read or patch
    /// rewrote between the fills.
    version: Vec<u64>,
    /// Each row's screen row at the memo's refresh before the one this frame was filled from,
    /// when it took the entry that row had, or [`NO_ROW`]: where the previous frame may hold
    /// its version.
    from: Vec<usize>,
    /// Whether `index` is of the rows as they are now: every write to them clears it.
    indexed: bool,
}

/// A frame's first two rows with one hash among its anchors (the strong vote's eligible rows)
/// and among its rows that say something (the weak vote's), each [`Places::NONE`] when absent.
/// A row is the only eligible one with its hash above `t` exactly when the first is above `t`
/// and the second is not, so one index serves every `t` of the frame.
#[derive(Debug, Clone, Copy, Default)]
struct Places([u32; 4]);

impl Places {
    const NONE: u32 = u32::MAX;
    const EMPTY: Self = Self([Self::NONE; 4]);

    /// Notes row `i` as eligible in `class` (0 strong, 1 weak).
    #[inline]
    fn note(&mut self, class: usize, i: u32) {
        let pair = &mut self.0[2 * class..2 * class + 2];
        if pair[0] == Self::NONE {
            pair[0] = i;
        } else if pair[1] == Self::NONE {
            pair[1] = i;
        }
    }

    /// The only row eligible in `class` above `t`, if exactly one is.
    #[inline]
    fn only(self, class: usize, t: usize) -> Option<usize> {
        let (first, second) = (self.0[2 * class] as usize, self.0[2 * class + 1] as usize);
        (first < t && second >= t).then_some(first)
    }
}

impl Frame {
    #[inline]
    const fn rows(&self) -> usize {
        self.text.len()
    }

    fn derive(&mut self) {
        self.indexed = false;
        self.hash.clear();
        self.hash.extend(self.text.iter().map(|s| row_hash(s)));
        self.version.clear();
        self.version.resize(self.text.len(), 0);
        self.from.clear();
        self.from.resize(self.text.len(), NO_ROW);
    }

    fn derive_votes(&mut self) {
        self.indexed = false;
        self.anchor.clear();
        self.content.clear();
        self.anchor
            .extend(self.text.iter().map(|s| is_anchor_text(s)));
        self.content
            .extend(self.text.iter().map(|s| is_content_text(s)));
    }

    /// Builds [`Self::index`] and [`Self::places`] if the rows changed since they were built.
    fn index(&mut self) {
        if self.indexed {
            return;
        }
        let rows = self.hash.len();
        self.index.clear();
        // Room for every row first, so no slot moves while the rows are noted.
        self.index.reserve(rows);
        self.slots.clear();
        self.slots.resize(rows, NO_ROW);
        let index = &mut self.index;
        let rows_of = self.hash.iter().zip(&self.anchor).zip(&self.content);
        for (i, (((&h, &strong), &weak), slot)) in (0u32..).zip(rows_of.zip(&mut self.slots)) {
            if strong || weak {
                let (at, places) = index.slot_mut(h, Places::EMPTY);
                if strong {
                    places.note(0, i);
                }
                if weak {
                    places.note(1, i);
                }
                *slot = at;
            }
        }
        self.places.clear();
        self.places.extend(self.slots.iter().map(|&slot| {
            if slot == NO_ROW {
                Places::EMPTY
            } else {
                self.index.value_at(slot)
            }
        }));
        self.indexed = true;
    }

    /// The only row above `t` eligible in `class` with hash `h`, if exactly one is; the index
    /// must be built.
    #[inline]
    fn only(&self, class: usize, h: u64, t: usize) -> Option<usize> {
        debug_assert!(self.indexed, "a frame's index is read only once built");
        self.index.get(h)?.only(class, t)
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

/// One row as a read of the grid saw it, with what it said.
#[derive(Debug)]
struct MemoRow {
    /// The written length it was read at, or [`RowMemo::UNREAD`]: never served.
    len: u32,
    text: String,
    hash: u64,
    anchor: bool,
    content: bool,
    /// Its text is its cells' low bytes, each a narrow ASCII char with no combining mark, with
    /// its trailing spaces cut ([`Read::Ascii`]): a change to some of its cells patches it.
    ascii: bool,
    /// The columns its cells that carried extras at its read lie in, joined with each span a
    /// patch since put a cell that carries extras in: none when the first is not below the
    /// last. No cell outside them carries extras.
    extras: (usize, usize),
    /// A number no other read or patch of any entry had ([`RowMemo::versions`]), given at its
    /// last: a frame row with it has its text ([`Frame::version`]).
    version: u64,
}

impl MemoRow {
    const fn unread() -> Self {
        Self {
            len: RowMemo::UNREAD,
            text: String::new(),
            hash: 0,
            anchor: false,
            content: false,
            ascii: false,
            extras: (0, 0),
            version: 0,
        }
    }

    /// Derives what an ASCII text says: it is normalized already, and only a space is a rule
    /// char in ASCII, so it is an anchor with four chars past its indent, and says something
    /// when it is not blank.
    fn derive_ascii(&mut self) {
        let indent = self.text.bytes().take_while(|&b| b == b' ').count();
        self.anchor = self.text.len() - indent >= MIN_ANCHOR_CHARS;
        self.content = !self.text.is_empty();
        self.hash = row_hash(&self.text);
    }
}

/// Where a screen row's cells are stored, the entry that holds what they read, and where it was
/// the refresh before.
#[derive(Debug, Clone, Copy)]
struct Seat {
    /// The address of its cells' storage: only a hint of where to look for its entry next time.
    addr: usize,
    /// Its entry ([`RowMemo::rows`]), or [`RowMemo::NONE`] while a refresh has not assigned it
    /// one.
    at: usize,
    /// Its screen row at the refresh before, when it took the entry that row had, or
    /// [`NO_ROW`] ([`Frame::from`]).
    from: usize,
}

/// The screen's rows as the last read of the grid saw them, so a commit re-reads only rows
/// whose cells changed, wherever a scroll moved them. A row's text is a function of its
/// written length and the cells up to it, and of the combining marks of those that carry
/// extras, which [`Grid::row_text_screen_into`] reads from beside the cells: a row with a
/// complex cell or a mark is never served from here. A row is served only when its cells equal
/// the ones it was read with. Where the grid stores them says which entry to compare, and the
/// grid's note of the cells written since the last read of that storage
/// ([`Grid::take_screen_rows_touched`]) says which of them to compare: every other cell up to
/// the written length it was read at is as that read left it, and has the marks it had then (a
/// mark's write notes its cell), so a row with no cell written since, at that length, is served
/// with no compare, and of a row whose cells carry extras only those written since that carry
/// them are looked up beside the cells. An ASCII row whose cells changed, or whose written
/// length did, is patched as it is visited: its written cells, and those past the length it was
/// read at, are narrowed into the text it read.
#[derive(Debug, Default)]
struct RowMemo {
    /// One entry per screen row, in no order.
    rows: Vec<MemoRow>,
    /// Each screen row's [`Seat`] at the last refresh: `rows[seats[r].at]` is screen row `r`.
    seats: Vec<Seat>,
    /// Entry `e`'s cells as bits ([`cell_bits`]) at `e * stride`.
    cells: Vec<u64>,
    stride: usize,
    /// A refresh's scratch: each screen row's [`Seat`] as it is visited and assigned.
    next: Vec<Seat>,
    /// A refresh's scratch: the entries a screen row already took.
    taken: Vec<bool>,
    /// A refresh's scratch: the screen rows that took an entry a patch could not bring up to
    /// date, for a read to redo whole.
    reread: Vec<usize>,
    /// A refresh's scratch: each ASCII screen row some of whose cells written since its last
    /// read carry extras, with the span of them, for [`Self::marks_clear`] to look at once the
    /// visit is done.
    marks: Vec<(usize, usize, usize)>,
    /// The last version given ([`MemoRow::version`]); never reset, so no two reads or patches
    /// share one.
    versions: u64,
    /// A planted divergence for the differential test's negative control: a patch keeps the
    /// version of the text it changed.
    #[cfg(test)]
    stale_version: bool,
    /// The old path, for the differential test: every row is re-read on every refresh.
    #[cfg(test)]
    bypass: bool,
    /// A planted divergence for the differential test's negative control: a row whose cells
    /// changed in place is served from its entry anyway, once it has one.
    #[cfg(test)]
    missed_row: Option<usize>,
    /// A planted divergence for the differential test's negative control: a frame the memo
    /// fills keeps the index of the rows it held before.
    #[cfg(test)]
    stale_index: bool,
    /// A planted divergence for the differential test's negative control: a patch keeps the
    /// text's old cut when the change reaches past it.
    #[cfg(test)]
    stale_cut: bool,
    /// A planted divergence for the differential test's negative control: a patch takes a
    /// span as saying what the text said when the text's own bytes agree, whatever the span
    /// writes past the text's cut.
    #[cfg(test)]
    lax_same: bool,
    /// A planted divergence for the differential test's negative control: no cell written since
    /// a row's last read is looked at for a combining mark beside it.
    #[cfg(test)]
    unchecked_marks: bool,
    /// A planted divergence for the differential test's negative control: a row that grew
    /// is patched with its changed cells alone, not those past the length it was read at.
    #[cfg(test)]
    short_growth: bool,
    /// Rows served without a re-read, those of them a scroll had moved, and rows patched, so
    /// the differential test can show what it compared.
    #[cfg(test)]
    hits: u64,
    #[cfg(test)]
    moved: u64,
    #[cfg(test)]
    patched: u64,
}

impl RowMemo {
    const UNREAD: u32 = u32::MAX;
    const NONE: usize = usize::MAX;

    /// Brings every row up to date with `grid`, re-reading only rows whose cells changed, and
    /// takes the grid's note of what was written since ([`Grid::take_screen_rows_touched`]).
    fn refresh(&mut self, grid: &mut Grid) {
        let rows = usize::from(grid.rows());
        let cols = usize::from(grid.cols());
        if self.stride != cols || self.rows.len() != rows {
            self.stride = cols;
            self.rows.clear();
            self.rows.resize_with(rows, MemoRow::unread);
            self.seats.clear();
            self.seats.extend((0..rows).map(|at| Seat {
                addr: 0,
                at,
                from: NO_ROW,
            }));
            self.cells.clear();
            self.cells.resize(rows * cols, 0);
        }
        self.next.clear();
        self.next.resize(
            rows,
            Seat {
                addr: 0,
                at: Self::NONE,
                from: NO_ROW,
            },
        );
        self.taken.clear();
        self.taken.resize(rows, false);
        self.reread.clear();
        self.marks.clear();
        let mut delta = 0;
        // A planted divergence may leave an entry's cells unlike those the grid did not touch.
        #[cfg(test)]
        let planted = self.missed_row.is_some() || self.short_growth;
        #[cfg(not(test))]
        let planted = false;
        grid.take_screen_rows_touched(|screen_row, slice, written_len, touched| {
            self.visit(screen_row, slice, written_len, touched, &mut delta, planted);
        });
        self.read_unserved(grid);
        std::mem::swap(&mut self.seats, &mut self.next);
    }

    /// Visits screen row `screen_row` for [`Self::refresh`]: its storage holds `slice`, the grid
    /// wrote `written_len` of its cells, and noted those in `touched` written since the last
    /// visit. It takes the entry that last read its storage ([`Self::find`], `delta` the move
    /// of the row visited before it), as it is when no cell was written since at the length it
    /// was read at, else brought up to date ([`Self::visit_written`]); a row that takes none
    /// here is read by [`Self::read_unserved`]. `planted`: a planted divergence may leave an
    /// entry's cells unlike those the grid did not note.
    #[inline]
    fn visit(
        &mut self,
        screen_row: u16,
        slice: &[Cell],
        written_len: u16,
        touched: std::ops::Range<usize>,
        delta: &mut isize,
        planted: bool,
    ) {
        let cols = self.stride;
        let r = usize::from(screen_row);
        let addr = slice.as_ptr().addr();
        let Some(seat) = self.next.get_mut(r) else {
            return;
        };
        seat.addr = addr;
        #[cfg(test)]
        if self.bypass {
            return;
        }
        let written = usize::from(written_len);
        #[cfg(test)]
        if self.missed_row == Some(r)
            && !self.taken[self.seats[r].at]
            && self.rows[self.seats[r].at].len != Self::UNREAD
        {
            seat.at = self.seats[r].at;
            self.taken[seat.at] = true;
            return;
        }
        let Some(e) = Self::find(&self.seats, r, addr, delta) else {
            return;
        };
        let row = &self.rows[e];
        // No cell written since its read, at the written length it was read at (an unread
        // entry's is none): it reads as it did.
        if touched.is_empty() && row.len == u32::from(written_len) && !self.taken[e] {
            debug_assert!(
                planted
                    || slice.get(..written).is_some_and(|cells| {
                        same_cells(cells, &self.cells[e * cols..e * cols + written])
                    }),
                "screen row {screen_row}: a cell the grid did not note as written changed"
            );
            seat.at = e;
            seat.from = r.wrapping_add_signed(*delta);
            self.taken[e] = true;
            #[cfg(test)]
            {
                self.hits += 1;
                self.moved += u64::from(e != self.seats[r].at);
            }
            return;
        }
        let Some(cells) = slice.get(..written).filter(|_| written <= cols) else {
            return;
        };
        if self.taken[e] || row.len == Self::UNREAD {
            return;
        }
        self.visit_written(r, cells, e, touched, *delta, planted);
    }

    /// Brings entry `e`, which no other row took and which read the storage of screen row `r`,
    /// up to date with `cells`, the row's written cells, of which those in `touched` were noted
    /// written since: serves it when none of them changed, else patches them in
    /// ([`Self::patch`]), and notes those written since that carry extras for
    /// [`Self::marks_clear`]. A row it cannot serve or patch takes no entry, or is queued for a
    /// read of the one it took. `delta` is the row's move since the last refresh.
    #[inline]
    fn visit_written(
        &mut self,
        r: usize,
        cells: &[Cell],
        e: usize,
        touched: std::ops::Range<usize>,
        delta: isize,
        planted: bool,
    ) {
        let row = &self.rows[e];
        let written = cells.len();
        // The written length it was read at: its cells up to there are the read's outside
        // the touched span.
        let had = row.len as usize;
        if had != written && !row.ascii {
            return;
        }
        let kept = had.min(written);
        let touched = touched.start.min(kept)..touched.end.min(kept);
        // Where the entry's cells up to `kept` are.
        let cols = self.stride;
        let snap = e * cols..e * cols + kept;
        debug_assert!(
            planted
                || same_cells(
                    &cells[..touched.start],
                    &self.cells[snap.clone()][..touched.start]
                ) && same_cells(
                    &cells[touched.end..kept],
                    &self.cells[snap.clone()][touched.end..]
                ),
            "screen row {r}: a cell the grid did not note as written changed"
        );
        let changed = if touched.is_empty() {
            None
        } else if row.ascii && touched.len() <= PATCH_TOUCHED {
            // Patched whole: the patch finds whether the text changed.
            Some((touched.start, touched.end))
        } else {
            match changed_span(&cells[touched.clone()], &self.cells[snap][touched.clone()]) {
                Some(_) if !row.ascii => return,
                span => span.map(|(first, last)| (touched.start + first, touched.start + last + 1)),
            }
        };
        // A row that grew has cells past the length it was read at, which the patch takes
        // whole; one that shrank is cut at its new length.
        #[cfg(test)]
        let short = self.short_growth;
        #[cfg(not(test))]
        let short = false;
        let span = if written > had && !short {
            Some((changed.map_or(had, |(first, _)| first.min(had)), written))
        } else if written != had {
            Some(changed.unwrap_or((written, written)))
        } else {
            changed
        };
        let seat = &mut self.next[r];
        seat.at = e;
        // Where the entry was: `find` leaves `delta` at its move.
        seat.from = r.wrapping_add_signed(delta);
        self.taken[e] = true;
        match span {
            None => {
                #[cfg(test)]
                {
                    self.hits += 1;
                    self.moved += u64::from(e != self.seats[r].at);
                }
            }
            // Its own storage, some cells changed or its length did: patch them in.
            Some((first, end)) => {
                if !self.patch(cells, e, first, end) {
                    self.reread.push(r);
                    return;
                }
            }
        }
        // The cells written since the last read: those touched and those the patch took. A
        // combining mark's write notes the cell it goes beside, so of all the row's cells
        // only these may have gained one without changing: those among the entry's that
        // carry extras ([`MemoRow::extras`]) are looked at.
        let (lo, hi) = match span {
            Some((first, end)) if touched.is_empty() => (first, end),
            Some((first, end)) => (first.min(touched.start), end.max(touched.end)),
            None => (touched.start, touched.end),
        };
        let (with, within) = self.rows[e].extras;
        let (lo, hi) = (lo.max(with), hi.min(within));
        #[cfg(test)]
        let hi = if self.unchecked_marks { lo } else { hi };
        if hi > lo {
            self.marks.push((r, lo, hi));
        }
    }

    /// Reads the rows a refresh's visit did not serve: each a patch could not bring up to date,
    /// and each with a combining mark now beside a cell written since its last read
    /// ([`Self::marks_clear`]), into the entry it took, then each that took none into an entry
    /// no row took. No ASCII row the memo holds then has a mark beside any of its cells.
    fn read_unserved(&mut self, grid: &Grid) {
        for i in 0..self.reread.len() {
            let r = self.reread[i];
            self.read(grid, r, self.next[r].at);
        }
        for i in 0..self.marks.len() {
            let (r, lo, hi) = self.marks[i];
            if !self.marks_clear(grid, r, self.next[r].at, lo..hi) {
                self.read(grid, r, self.next[r].at);
            }
        }
        let mut spare = 0;
        for r in 0..self.next.len() {
            if self.next[r].at != Self::NONE {
                continue;
            }
            while self.taken[spare] {
                spare += 1;
            }
            self.taken[spare] = true;
            self.next[r].at = spare;
            self.read(grid, r, spare);
        }
        #[cfg(test)]
        let planted = self.missed_row.is_some() || self.unchecked_marks || self.short_growth;
        #[cfg(not(test))]
        let planted = false;
        #[cfg(debug_assertions)]
        for (r, &Seat { at: e, .. }) in self.next.iter().enumerate() {
            let len = self.rows[e].len;
            debug_assert!(
                planted
                    || !self.rows[e].ascii
                    || len == Self::UNREAD
                    || self.marks_clear(grid, r, e, 0..len as usize),
                "screen row {r}: a combining mark beside a cell the grid did not note as written"
            );
        }
        let _ = planted;
    }

    /// Whether no cell `cols` of entry `e`, which screen row `r` took, carries a combining
    /// mark: those that carry extras are looked up beside them as [`read_cells`] looks them up.
    fn marks_clear(&self, grid: &Grid, r: usize, e: usize, cols: std::ops::Range<usize>) -> bool {
        let at = e * self.stride;
        let (Ok(screen_row), Some(words)) = (
            u16::try_from(r),
            self.cells.get(at + cols.start..at + cols.end),
        ) else {
            return false;
        };
        (cols.start..).zip(words).all(|(col, &bits)| {
            bits & EXTRAS_CELL == 0
                || u16::try_from(col).is_ok_and(|col| {
                    grid.cell_extra(screen_row, col)
                        .is_none_or(|extra| extra.combining().is_empty())
                })
        })
    }

    /// The entry of `seats`, the last refresh's, that held the row stored at `addr`, looked for
    /// first where the row before it moved from (`delta` rows away), then in place, then
    /// everywhere.
    #[inline]
    fn find(seats: &[Seat], r: usize, addr: usize, delta: &mut isize) -> Option<usize> {
        if let Some(seat) = r
            .checked_add_signed(*delta)
            .and_then(|j| seats.get(j))
            .filter(|seat| seat.addr == addr)
        {
            return Some(seat.at);
        }
        if let Some(seat) = seats.get(r).filter(|seat| seat.addr == addr) {
            *delta = 0;
            return Some(seat.at);
        }
        let j = seats.iter().position(|seat| seat.addr == addr)?;
        *delta = j.cast_signed() - r.cast_signed();
        Some(seats[j].at)
    }

    /// Patches cells `first..end` of `cells`, a screen row's written cells, into entry `e`, an
    /// ASCII read of the same storage: those that may have changed since it, and those past the
    /// length it was read at. Its text is its cells' low bytes with trailing spaces cut, so only
    /// the span's bytes, the bytes cut off past the new length and the cut can change; a patch
    /// that leaves the text as it was leaves its version too. A span cell that carries extras
    /// notes the entry's ([`MemoRow::extras`]). False when a cell of the span is not a narrow
    /// ASCII char, for a read to redo the entry whole, its cells' bits and its text with it.
    #[inline(never)]
    fn patch(&mut self, cells: &[Cell], e: usize, first: usize, end: usize) -> bool {
        let cols = self.stride;
        let (Some(span), Some(bits), Some(row), Ok(len)) = (
            cells.get(first..end),
            self.cells.get_mut(e * cols + first..e * cols + end),
            self.rows.get_mut(e),
            u32::try_from(cells.len()),
        ) else {
            return false;
        };
        // The text is ASCII, one byte a cell: it says its own bytes up to its cut and blanks
        // past it, and nothing past the written length. The span's cells up to the bytes the new
        // length keeps (`inside`) are patched in place, the rest put after those bytes.
        let cut = row.text.len();
        let kept = cut.min(cells.len());
        let inside = kept.clamp(first, end) - first;
        let (span_in, span_past) = span.split_at(inside);
        let (bits_in, bits_past) = bits.split_at_mut(inside);
        // SAFETY: the text is ASCII (an ASCII read's, or a patch's of one) and so is every byte
        // written into it, failed patch or not: `narrow` gives a byte below 0x7f, and the blanks
        // put before the span are spaces. A byte below 0x80 is a whole char, so the text stays
        // UTF-8, cut anywhere.
        let text = unsafe { row.text.as_mut_vec() };
        let at = first.min(kept);
        // One pass over the span: the entry takes each cell's bits and the text its byte, and
        // `diff` notes a byte inside that changed.
        let (mut any, mut diff) = (0, 0);
        for ((b, byte), c) in bits_in
            .iter_mut()
            .zip(&mut text[at..at + inside])
            .zip(span_in)
        {
            *b = cell_bits(*c);
            any |= *b;
            let new = narrow(*b);
            diff |= *byte ^ new;
            *byte = new;
        }
        text.truncate(kept);
        if !span_past.is_empty() {
            text.resize(first.max(kept), b' ');
            text.extend(bits_past.iter_mut().zip(span_past).map(|(b, c)| {
                *b = cell_bits(*c);
                any |= *b;
                narrow(*b)
            }));
        }
        if any & ODD_CELL != 0 {
            return false;
        }
        row.len = len;
        if any & EXTRAS_CELL != 0 {
            let (lo, hi) = row.extras;
            row.extras = if lo < hi {
                (lo.min(first), hi.max(end))
            } else {
                (first, end)
            };
        }
        #[cfg(test)]
        if self.lax_same && diff == 0 && kept == cut {
            text.truncate(cut);
            self.patched += 1;
            return true;
        }
        // A text that ended past the change, and kept its length, still ends where it did;
        // else its end is found again, at or before the change or the new length.
        #[cfg(test)]
        let stale_cut = self.stale_cut;
        #[cfg(not(test))]
        let stale_cut = false;
        if !stale_cut && (end >= kept || kept < cut) {
            text.truncate(trimmed_len(text));
        }
        #[cfg(test)]
        {
            self.patched += 1;
        }
        // No byte inside changed and the end is where it was (the span put only blanks past
        // the cut): the text, and so its version, are as they were.
        if diff == 0 && text.len() == cut {
            return true;
        }
        row.derive_ascii();
        #[cfg(test)]
        let stale = self.stale_version;
        #[cfg(not(test))]
        let stale = false;
        if !stale {
            self.versions += 1;
            row.version = self.versions;
        }
        true
    }

    /// Reads screen row `r` into entry `e`, keeping its cells when they alone say what it
    /// reads.
    fn read(&mut self, grid: &Grid, r: usize, e: usize) {
        let cols = self.stride;
        let Ok(screen_row) = u16::try_from(r) else {
            return;
        };
        let view = grid.screen_row_view(screen_row);
        let written_len = view.written_len();
        let written = usize::from(written_len);
        let slice = view.cells_slice();
        #[cfg(test)]
        let bypass = self.bypass;
        #[cfg(not(test))]
        let bypass = false;
        let row = &mut self.rows[e];
        self.versions += 1;
        row.version = self.versions;
        self.next[r].addr = slice.as_ptr().addr();
        let (read, marked) = match slice.get(..written).filter(|_| written <= cols && !bypass) {
            Some(cells) => {
                let words = &mut self.cells[e * cols..e * cols + written];
                let mut any = 0;
                for (bits, c) in words.iter_mut().zip(cells) {
                    *bits = cell_bits(*c);
                    any |= *bits;
                }
                read_cells(cells, words, any, grid, screen_row, &mut row.text)
            }
            None => (Read::Complex, false),
        };
        // A row with a complex cell or a combining mark is never served; one with extras and
        // no mark reads as ASCII, and is served while none appears ([`RowMemo::marks_clear`]).
        row.len = if read == Read::Complex || marked && read != Read::Ascii {
            Self::UNREAD
        } else {
            u32::from(written_len)
        };
        if read == Read::Complex {
            grid.row_text_screen_into(screen_row, &mut row.text);
        }
        row.ascii = read == Read::Ascii;
        row.extras = if marked {
            extras_span(&self.cells[e * cols..e * cols + written])
        } else {
            (0, 0)
        };
        if row.ascii {
            row.derive_ascii();
        } else {
            normalize_row(&mut row.text);
            row.anchor = is_anchor_text(&row.text);
            row.content = is_content_text(&row.text);
            row.hash = row_hash(&row.text);
        }
    }

    /// Writes the screen as the last refresh read it into `frame`, derived.
    fn fill(&self, frame: &mut Frame) {
        #[cfg(test)]
        let stale = self.stale_index;
        #[cfg(not(test))]
        let stale = false;
        frame.indexed &= stale;
        let rows = self.seats.len();
        frame.text.resize_with(rows, String::new);
        frame.hash.resize(rows, 0);
        frame.anchor.resize(rows, false);
        frame.content.resize(rows, false);
        frame.version.resize(rows, 0);
        frame.from.resize(rows, NO_ROW);
        let fields = (frame.hash.iter_mut().zip(&mut frame.anchor))
            .zip(frame.content.iter_mut().zip(&mut frame.version));
        for (((text, from), ((hash, anchor), (content, version))), seat) in
            (frame.text.iter_mut().zip(&mut frame.from))
                .zip(fields)
                .zip(&self.seats)
        {
            let row = &self.rows[seat.at];
            text.clone_from(&row.text);
            *from = seat.from;
            *hash = row.hash;
            *anchor = row.anchor;
            *content = row.content;
            *version = row.version;
        }
    }

    /// Hands back the memory of a screen no longer committed.
    fn release(&mut self) {
        *self = Self {
            #[cfg(test)]
            bypass: self.bypass,
            #[cfg(test)]
            missed_row: self.missed_row,
            #[cfg(test)]
            stale_index: self.stale_index,
            #[cfg(test)]
            stale_cut: self.stale_cut,
            #[cfg(test)]
            lax_same: self.lax_same,
            #[cfg(test)]
            unchecked_marks: self.unchecked_marks,
            #[cfg(test)]
            short_growth: self.short_growth,
            #[cfg(test)]
            stale_version: self.stale_version,
            versions: self.versions,
            #[cfg(test)]
            hits: self.hits,
            #[cfg(test)]
            moved: self.moved,
            #[cfg(test)]
            patched: self.patched,
            ..Self::default()
        };
    }
}

/// How [`read_cells`] read a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Every cell a narrow ASCII char with no combining mark: the text is normalized already.
    Ascii,
    /// From the cells, and the combining marks of those with extras.
    Plain,
    /// Not read: a complex cell, whose text only the grid's reader assembles.
    Complex,
}

/// Reads `cells`, the first [written](crate::grid::row::Row::written_len) cells of screen row
/// `screen_row`, with `words` their [`cell_bits`] and `any` those or'd together, into `out` as
/// [`Grid::row_text_screen_into`] does (a cell is its char, NUL a space, then its combining
/// marks; a wide head's continuation is skipped), unless one is complex ([`Read::Complex`], `out`
/// unspecified). An all-ASCII row ([`Read::Ascii`]) comes out as [`normalize_row`] would leave
/// it: each control char a space, trailing spaces cut. Also says whether a cell carries extras,
/// whose marks the cells alone do not say.
fn read_cells(
    cells: &[Cell],
    words: &[u64],
    any: u64,
    grid: &Grid,
    screen_row: u16,
    out: &mut String,
) -> (Read, bool) {
    debug_assert_eq!(
        any,
        words.iter().fold(0, |any, w| any | w),
        "`any` is the words or'd"
    );
    let marked = any & EXTRAS_CELL != 0;
    let marks = |col: usize| {
        u16::try_from(col)
            .ok()
            .and_then(|col| grid.cell_extra(screen_row, col))
            .map_or(&[][..], |extra| extra.combining())
    };
    if any & ODD_CELL == 0
        && (!marked
            || (words.iter().enumerate())
                .all(|(col, w)| w & EXTRAS_CELL == 0 || marks(col).is_empty()))
    {
        // One byte a cell, a control char (NUL among them) a space. Narrowed from the
        // contiguous words, which vectorizes; a field read from the packed cells does not.
        let mut bytes = std::mem::take(out).into_bytes();
        bytes.clear();
        bytes.extend(words.iter().map(|&w| narrow(w)));
        bytes.truncate(trimmed_len(&bytes));
        if let Ok(text) = String::from_utf8(bytes) {
            *out = text;
            return (Read::Ascii, marked);
        }
    }
    out.clear();
    out.reserve(cells.len());
    let mut after_wide = false;
    for (col, c) in cells.iter().enumerate() {
        if c.is_complex() {
            return (Read::Complex, marked);
        }
        let wide = c.is_wide();
        if !(c.is_wide_continuation() && !wide && after_wide) {
            let ch = c.char();
            out.push(if ch == '\0' { ' ' } else { ch });
            if c.has_extras() {
                out.extend(marks(col));
            }
        }
        after_wide = wide;
    }
    (Read::Plain, marked)
}

/// The bits of a cell ([`cell_bits`]) whose any one makes its text other than one ASCII byte
/// (and its marks): a char past ASCII, a complex char, a wide head's or continuation's flag.
const ODD_CELL: u64 = 0xff80
    | (((CellFlags::COMPLEX.bits() | CellFlags::WIDE.bits() | CellFlags::WIDE_CONTINUATION.bits())
        as u64)
        << 48);

/// The bit of a cell ([`cell_bits`]) that says it carries extras.
const EXTRAS_CELL: u64 = (PackedColors(0).with_extras_flag().0 as u64) << 16;

/// The columns of `words`, cells' bits ([`cell_bits`]), whose cells carry extras: from the first
/// to past the last, or none.
fn extras_span(words: &[u64]) -> (usize, usize) {
    let has = |w: &u64| w & EXTRAS_CELL != 0;
    match (words.iter().position(has), words.iter().rposition(has)) {
        (Some(first), Some(last)) => (first, last + 1),
        _ => (0, 0),
    }
}

/// A cell's eight bytes as one word, every field in it: two cells with equal bits say the same.
#[inline]
fn cell_bits(c: Cell) -> u64 {
    u64::from(c.char_data()) | (u64::from(c.colors().0) << 16) | (u64::from(c.flags().bits()) << 48)
}

#[inline]
fn same_cells(cells: &[Cell], snap: &[u64]) -> bool {
    cells.len() == snap.len()
        && cells
            .iter()
            .zip(snap)
            .fold(0, |diff, (c, &s)| diff | (cell_bits(*c) ^ s))
            == 0
}

/// The byte an ASCII cell's bits ([`cell_bits`]) read as: its char, a control char (NUL among
/// them) a space. Any other cell's is below 0x7f too (a space past it), so a byte it gives is a
/// whole char whatever cell it came from.
#[inline]
const fn narrow(bits: u64) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the char's low byte is the whole char below 0x80"
    )]
    match bits as u8 {
        b if b < 0x20 || b >= 0x7f => b' ',
        b => b,
    }
}

/// How long `bytes` is without its trailing spaces. Sixteen bytes at a time from the end, which
/// vectorizes; a byte at a time does not.
#[inline]
fn trimmed_len(bytes: &[u8]) -> usize {
    let last = |b: &[u8]| b.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    let (head, chunks) = bytes.as_rchunks::<16>();
    chunks
        .iter()
        .rposition(|chunk| *chunk != [b' '; 16])
        .map_or_else(|| last(head), |k| head.len() + 16 * k + last(&chunks[k]))
}

/// The first and last of `cells` whose bits differ from `snap`, as long, or `None` when none
/// does. Eight cells at a time from each end, which vectorizes.
fn changed_span(cells: &[Cell], snap: &[u64]) -> Option<(usize, usize)> {
    let n = cells.len().min(snap.len());
    let differs = |i: usize| cell_bits(cells[i]) != snap[i];
    let block = |at: usize| {
        cells[at..at + 8]
            .iter()
            .zip(&snap[at..at + 8])
            .fold(0, |diff, (c, &s)| diff | (cell_bits(*c) ^ s))
            != 0
    };
    let whole = n - n % 8;
    let first = (0..whole).step_by(8).find(|&at| block(at)).map_or_else(
        || (whole..n).find(|&i| differs(i)),
        |at| (at..at + 8).find(|&i| differs(i)),
    )?;
    let last = (whole..n).rev().find(|&i| differs(i)).or_else(|| {
        (0..whole)
            .step_by(8)
            .rev()
            .find(|&at| block(at))
            .and_then(|at| (at..at + 8).rev().find(|&i| differs(i)))
    })?;
    Some((first, last))
}

/// Reused per-commit scratch (maps keep their capacity across frames).
#[derive(Debug, Default)]
struct Scratch {
    /// The last vote's `t` and class (0 strong, 1 weak): which rows are "the only one with
    /// its hash" to the lookups that follow it in the same commit.
    voted: (usize, usize),
    /// The old path, for the differential test: each vote indexes both frames' eligible rows
    /// above `t` afresh, and a reanchor scans the archive's tail.
    #[cfg(test)]
    reference: bool,
    /// A planted divergence for the differential test's negative control: the tail index is
    /// built once and never catches up with the rows archived after.
    #[cfg(test)]
    stale_tail: bool,
    /// The reference path's indexes: each eligible hash's count above `t` and its first row.
    #[cfg(test)]
    prev_idx: HashIndex<(u32, usize)>,
    #[cfg(test)]
    cur_idx: HashIndex<(u32, usize)>,
    /// A vote's tallies, the strong then the weak class's, each indexed by `d + T - 1`: (votes,
    /// cur positions of the topmost and lowest voter). Zero between votes.
    votes: Vec<(u32, usize, usize)>,
    /// A reanchor's scratch: each cur row's last archived row with its hash, or [`NO_ROW`].
    tail_pos: Vec<usize>,
    j_votes: HashIndex<u32>,
    picks: Vec<usize>,
    /// An in-place commit's scratch: the screen rows whose text may have changed.
    changed: Vec<usize>,
    /// Planted divergences for the differential test's negative controls: an in-place commit
    /// takes a changed row whose text another row had as voting for no move (`lax_moves`), or
    /// counts every unchanged anchor as voting for none (`lax_voters`).
    #[cfg(test)]
    lax_moves: bool,
    #[cfg(test)]
    lax_voters: bool,
    /// Commits made in place, so the differential test can show it compared them.
    #[cfg(test)]
    in_place: u64,
}

/// The most touched cells of an ASCII row a refresh patches without first finding the ones
/// that changed ([`changed_span`]): a span that wide or narrower is mostly changed cells.
const PATCH_TOUCHED: usize = 32;

/// No row: an empty [`Scratch::tail_pos`] slot.
const NO_ROW: usize = usize::MAX;

/// The archive's anchor rows by hash, each hash's newest (an absolute index), so a reanchor
/// looks its cur rows up rather than scanning the tail. It holds every anchor row from `from`
/// up to `to`. Archived rows never change and are only appended, so catching up is indexing
/// the rows after `to`; a row that left the front is newer than nothing after it.
#[derive(Debug, Default)]
struct TailIndex {
    newest: HashIndex<u64>,
    from: u64,
    to: u64,
}

/// A map from `u64` keys (row hashes, offsets) to small values, rebuilt every commit: open
/// addressing over a power-of-two table at most half full, emptied by a new stamp rather than
/// by a sweep.
#[derive(Debug, Default)]
struct HashIndex<V> {
    /// `(key, stamp, value)`: a slot holds an entry when its stamp is `stamp`.
    slots: Vec<(u64, u32, V)>,
    stamp: u32,
    used: usize,
}

impl<V: Copy + Default> HashIndex<V> {
    /// Empties the index.
    fn clear(&mut self) {
        self.used = 0;
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            for slot in &mut self.slots {
                slot.1 = 0;
            }
            self.stamp = 1;
        }
    }

    /// Where `key`'s probe starts: the top bits of `key` times an odd constant.
    #[inline]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the top `bits` bits are below the table's length, a usize"
    )]
    fn home(&self, key: u64) -> usize {
        let bits = self.slots.len().trailing_zeros();
        (key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (64 - bits)) as usize
    }

    /// The value at `key`.
    #[inline]
    fn get(&self, key: u64) -> Option<V> {
        if self.used == 0 {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut i = self.home(key);
        loop {
            let (k, stamp, v) = self.slots[i];
            if stamp != self.stamp {
                return None;
            }
            if k == key {
                return Some(v);
            }
            i = (i + 1) & mask;
        }
    }

    /// The value at `key`, inserted as `fresh` when absent, and whether it was.
    #[inline]
    fn entry(&mut self, key: u64, fresh: V) -> (&mut V, bool) {
        self.reserve(1);
        let used = self.used;
        let i = self.slot(key, fresh);
        (&mut self.slots[i].2, self.used > used)
    }

    /// Makes room for `n` more entries, so the slots of the next `n` inserted stay put.
    #[inline]
    fn reserve(&mut self, n: usize) {
        while 2 * (self.used + n) > self.slots.len() {
            self.grow();
        }
    }

    /// The slot holding `key`, inserted as `fresh` when absent; room for it must be
    /// [reserved](Self::reserve).
    #[inline]
    fn slot(&mut self, key: u64, fresh: V) -> usize {
        self.slot_mut(key, fresh).0
    }

    /// [`Self::slot`], with its value.
    #[inline]
    fn slot_mut(&mut self, key: u64, fresh: V) -> (usize, &mut V) {
        debug_assert!(
            2 * (self.used + 1) <= self.slots.len(),
            "a slot is reserved first"
        );
        let mask = self.slots.len() - 1;
        let stamp = self.stamp;
        let mut i = self.home(key);
        let i = loop {
            let slot = &mut self.slots[i];
            if slot.1 != stamp {
                *slot = (key, stamp, fresh);
                self.used += 1;
                break i;
            }
            if slot.0 == key {
                break i;
            }
            i = (i + 1) & mask;
        };
        (i, &mut self.slots[i].2)
    }

    /// The value in slot `i`, which [`Self::slot`] gave.
    #[inline]
    fn value_at(&self, i: usize) -> V {
        self.slots[i].2
    }

    /// Doubles the table (at least 64 slots), keeping its entries.
    fn grow(&mut self) {
        let old = std::mem::take(&mut self.slots);
        let stamp = self.stamp;
        self.slots = vec![(0, 0, V::default()); (2 * old.len()).max(64)];
        self.stamp = 1;
        self.used = 0;
        for (k, s, v) in old {
            if s == stamp {
                *self.entry(k, v).0 = v;
            }
        }
    }

    /// Every entry, in no order.
    fn iter(&self) -> impl Iterator<Item = (u64, V)> + '_ {
        self.slots
            .iter()
            .filter(|s| s.1 == self.stamp)
            .map(|&(k, _, v)| (k, v))
    }
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
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the minimum byte floor allotted to each active archive in this pool.
    #[inline]
    #[must_use]
    pub const fn min_share(&self) -> usize {
        self.min_share
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
    /// The retained rows' text.
    store: TextStore,
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
    below: VecDeque<HeldRow>,
    /// Lowest index a reanchor or a scroll-back may point at: rows archived
    /// before the app's current alt-screen run (a previous app, across a
    /// leave/reset/restore gap) are never the rows this screen shows again.
    floor: u64,
    memo: RowMemo,
    scratch: Scratch,
    tail: TailIndex,
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
            store: TextStore::default(),
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
            memo: RowMemo::default(),
            scratch: Scratch::default(),
            tail: TailIndex::default(),
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
        self.rows.get(off).map(|r| self.store.text(r))
    }

    /// Returns all retained row strings, oldest first.
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|r| self.store.text(r).to_string())
            .collect()
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
            .map(|r| Arc::from(self.store.text(r)))
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
        self.cur.derive();
        self.cur.derive_votes();
        self.commit_prepared(cols, None);
    }

    fn is_unchanged(&self, key: u64, rows: usize, cols: u16) -> bool {
        self.have_prev
            && self.prev_key == Some(key)
            && self.prev.rows() == rows
            && self.prev.cols == cols
    }

    #[cfg(test)]
    fn frame_buffers(&mut self, rows: usize) -> &mut [String] {
        self.cur.text.resize_with(rows, String::new);
        &mut self.cur.text
    }

    /// Reads the grid's screen into `cur`, derived.
    fn fill_from_grid(&mut self, grid: &mut Grid) {
        self.memo.refresh(grid);
        self.memo.fill(&mut self.cur);
    }

    fn commit_grid(&mut self, grid: &mut Grid) {
        let rows = grid.rows();
        let cols = grid.cols();
        if rows == 0 || cols == 0 {
            return;
        }
        let key = grid.content_gen();
        if self.is_unchanged(key, usize::from(rows), cols) {
            return;
        }
        self.memo.refresh(grid);
        if self.commit_in_place(cols, key) {
            return;
        }
        self.memo.fill(&mut self.cur);
        self.commit_prepared(cols, Some(key));
    }

    /// Commits the screen the memo holds as [`Self::commit_prepared`] would, when it differs
    /// from the previous frame in at most [`IN_PLACE_ROWS`] rows and the diff can only confirm
    /// it where it stands: no changed row's text is another row's in the previous frame, and at
    /// least as many unchanged rows as a vote needs are anchors no other row shares. Then no row
    /// votes for a move, those vote for none, and the diff narrows the chrome and keeps the
    /// debt it still shows. The previous frame takes the changed rows where it stands: no copy
    /// of the rest, no index and no vote. False, with nothing changed, when the frame needs the
    /// whole diff.
    fn commit_in_place(&mut self, cols: u16, key: u64) -> bool {
        #[cfg(test)]
        if self.scratch.reference {
            return false;
        }
        let rows = self.memo.seats.len();
        if !self.have_prev
            || self.prev.cols != cols
            || self.prev.rows() != rows
            || !self.below.is_empty()
        {
            return false;
        }
        // The rows the previous frame may not show as they are now: those whose entry's version
        // is not the one it holds there (one version is one text, [`MemoRow::version`]).
        let mut changed = std::mem::take(&mut self.scratch.changed);
        changed.clear();
        let fits = (0..rows).all(|r| {
            let version = self.memo.rows[self.memo.seats[r].at].version;
            if version != 0 && version == self.prev.version[r] {
                return true;
            }
            changed.push(r);
            changed.len() <= IN_PLACE_ROWS
        });
        let done = fits && self.confirm_in_place(&changed, key);
        self.scratch.changed = changed;
        if done {
            // As a commit leaves it: holding a frame of the screen's rows.
            self.cur.text.resize_with(rows, String::new);
            #[cfg(test)]
            {
                self.scratch.in_place += 1;
            }
        }
        done
    }

    /// [`Self::commit_in_place`] of the screen whose rows other than `changed` (ascending) are
    /// the previous frame's.
    fn confirm_in_place(&mut self, changed: &[usize], key: u64) -> bool {
        let memo = &self.memo;
        let now = |r: usize| &memo.rows[memo.seats[r].at];
        let prev = &self.prev;
        let blank = |r: usize| {
            if changed.contains(&r) {
                now(r).text.is_empty()
            } else {
                prev.text[r].is_empty()
            }
        };
        if (0..prev.rows()).all(blank) {
            self.evict();
            return true;
        }
        let Some(&last) = changed.iter().rev().find(|&&r| now(r).hash != prev.hash[r]) else {
            self.evict();
            self.prev_key = Some(key);
            return true;
        };
        let t = last + 1;
        let need = self.need(t, false);
        // An anchor of the previous frame above `t` other than row `i` with hash `h`.
        let other_anchor = |h: u64, i: usize| {
            (prev.hash[..t].iter().zip(&prev.anchor[..t]))
                .enumerate()
                .any(|(j, (&hj, &a))| a && hj == h && j != i)
        };
        #[cfg(test)]
        let (lax_moves, lax_voters) = (self.scratch.lax_moves, self.scratch.lax_voters);
        #[cfg(not(test))]
        let (lax_moves, lax_voters) = (false, false);
        // A changed row that is an anchor whose hash another row had could vote for a move.
        if !lax_moves
            && changed
                .iter()
                .any(|&r| r < t && now(r).anchor && other_anchor(now(r).hash, r))
        {
            return false;
        }
        // An unchanged anchor that no other row had, and that no changed row had or has, is the
        // only row with its text in both frames: it votes for no move.
        let shared = |h: u64| {
            changed
                .iter()
                .any(|&r| prev.hash[r] == h || now(r).hash == h)
        };
        let mut voters = 0;
        for i in 0..t {
            if voters == need {
                break;
            }
            if prev.anchor[i]
                && !changed.contains(&i)
                && (lax_voters || !shared(prev.hash[i]) && !other_anchor(prev.hash[i], i))
            {
                voters += 1;
            }
        }
        if voters < need {
            return false;
        }
        self.evict();
        self.scratch.voted = (t, 0);
        self.narrow_chrome(self.prev.rows() - t);
        for &r in changed {
            let row = &self.memo.rows[self.memo.seats[r].at];
            self.prev.text[r].clone_from(&row.text);
            self.prev.hash[r] = row.hash;
            self.prev.anchor[r] = row.anchor;
            self.prev.content[r] = row.content;
            self.prev.version[r] = row.version;
        }
        self.prev.indexed = false;
        self.debt = self.debt_shown(t, &self.prev.hash);
        self.prev_key = Some(key);
        true
    }

    /// Commits `cur`, already derived.
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
        if self.have_prev
            && self.prev.cols == cols
            && self.prev.rows() == rows
            && self.prev.hash == self.cur.hash
        {
            self.prev_key = key;
            return;
        }
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

    fn prev_region(&self) -> usize {
        let r = self.prev.rows();
        r - self.chrome.unwrap_or(0).min(r)
    }

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

    fn same_across_resize(&self, i: usize, j: usize) -> bool {
        self.prev.hash[i] == self.cur.hash[j]
            || said_text(&self.prev.text[i]).eq(said_text(&self.cur.text[j]))
    }

    fn narrow_chrome(&mut self, b: usize) {
        self.chrome = Some(self.chrome.map_or(b, |c| c.min(b)));
    }

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

    fn diff(&mut self, t: usize, b: usize) {
        let rows = self.cur.rows();
        let [(best, moved), (weak, _)] = self.votes(t);
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
            self.voted(t, 1);
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

    /// The only row of the current frame above the last vote's `t` eligible in its class with
    /// hash `h`, if exactly one is.
    #[inline]
    fn only_cur(&self, h: u64) -> Option<usize> {
        let (t, class) = self.scratch.voted;
        #[cfg(test)]
        if self.scratch.reference {
            return self
                .scratch
                .cur_idx
                .get(h)
                .and_then(|(n, i)| (n == 1).then_some(i));
        }
        self.cur.only(class, h, t)
    }

    /// [`Self::only_cur`] of cur row `i`'s own hash: its own [`Places`] say it, with no lookup
    /// (a row eligible in either class has its hash's).
    #[inline]
    fn only_cur_row(&self, i: usize) -> Option<usize> {
        #[cfg(test)]
        if self.scratch.reference {
            return self.only_cur(self.cur.hash[i]);
        }
        let (t, class) = self.scratch.voted;
        self.cur.places[i].only(class, t)
    }

    /// [`Self::only_cur`] of the previous frame.
    #[inline]
    fn only_prev(&self, h: u64) -> Option<usize> {
        let (t, class) = self.scratch.voted;
        #[cfg(test)]
        if self.scratch.reference {
            return self
                .scratch
                .prev_idx
                .get(h)
                .and_then(|(n, i)| (n == 1).then_some(i));
        }
        self.prev.only(class, h, t)
    }

    /// Makes the rows of class `class` (0 strong, 1 weak) above `t` the eligible ones to the
    /// lookups that follow a vote ([`Self::only_cur`], [`Self::only_prev`]).
    fn voted(&mut self, t: usize, class: usize) {
        self.scratch.voted = (t, class);
        #[cfg(test)]
        if self.scratch.reference {
            let (prev_ok, cur_ok) = if class == 1 {
                (&self.prev.content, &self.cur.content)
            } else {
                (&self.prev.anchor, &self.cur.anchor)
            };
            index_anchors(&self.prev.hash, prev_ok, t, &mut self.scratch.prev_idx);
            index_anchors(&self.cur.hash, cur_ok, t, &mut self.scratch.cur_idx);
        }
    }

    /// The strong and the weak vote of the rows above `t`, each its best move and its best
    /// move other than none, leaving the strong one's rows eligible ([`Self::voted`]). A cur
    /// row votes in a class for the offset to the previous frame's row with its text when each
    /// is the only row eligible in that class with its hash above `t`. One pass serves both
    /// classes: a cur row's own [`Places`] say whether it is the only one, and one lookup in
    /// the previous frame finds its match in each.
    fn votes(&mut self, t: usize) -> [(Vote, Vote); 2] {
        #[cfg(test)]
        if self.scratch.reference {
            let weak = self.reference_vote(t, 1);
            return [self.reference_vote(t, 0), weak];
        }
        self.voted(t, 0);
        self.prev.index();
        self.cur.index();
        // Each class's tally, indexed by `d + t - 1`, zero again once read.
        let width = 2 * t - 1;
        let mut tally = std::mem::take(&mut self.scratch.votes);
        if tally.len() < 2 * width {
            tally.resize(2 * width, (0, 0, 0));
        }
        let mut span = [(usize::MAX, 0); 2];
        let (cur, prev) = (&self.cur, &self.prev);
        let rows = (cur.places[..t].iter().zip(&cur.from[..t]))
            .zip(cur.version[..t].iter().zip(&cur.hash[..t]));
        for (i, ((&places, &j), (&version, &hash))) in rows.enumerate() {
            let only = [places.only(0, t) == Some(i), places.only(1, t) == Some(i)];
            if !(only[0] || only[1]) {
                continue;
            }
            // A row whose version the previous frame holds at `j`, the row it came from, has
            // `j`'s text: `j`'s places are its hash's, and `j` matches it without a compare
            // ([`Frame::version`]). `compared` is the previous row last compared, and whether
            // its text was this row's.
            let (theirs, mut compared) = if version != 0 && prev.version.get(j) == Some(&version) {
                (prev.places[j], (j, true))
            } else {
                let Some(theirs) = prev.index.get(hash) else {
                    continue;
                };
                (theirs, (NO_ROW, false))
            };
            for class in 0..2 {
                let Some(p) = theirs.only(class, t).filter(|_| only[class]) else {
                    continue;
                };
                if compared.0 != p {
                    compared = (p, prev.text[p] == cur.text[i]);
                }
                if !compared.1 {
                    continue;
                }
                let k = p + t - 1 - i;
                let slot = &mut tally[class * width + k];
                if slot.0 == 0 {
                    slot.1 = i;
                }
                slot.0 += 1;
                slot.2 = i;
                span[class] = (span[class].0.min(k), span[class].1.max(k));
            }
        }
        let mut out = [(Vote::default(), Vote::default()); 2];
        for (class, (best, moved)) in out.iter_mut().enumerate() {
            let (lo, hi) = span[class];
            if lo > hi {
                continue;
            }
            for (k, slot) in (lo..).zip(&mut tally[class * width + lo..=class * width + hi]) {
                let (v, top, bot) = std::mem::take(slot);
                if v == 0 {
                    continue;
                }
                let cand = Vote {
                    d: k.cast_signed() - (t - 1).cast_signed(),
                    votes: v as usize,
                    top,
                    bot,
                };
                if cand.beats(*best) {
                    *best = cand;
                }
                if cand.d != 0 && cand.beats(*moved) {
                    *moved = cand;
                }
            }
        }
        self.scratch.votes = tally;
        out
    }

    /// The old path's vote of class `class` above `t`, for the differential test: each frame's
    /// eligible rows indexed afresh, a lookup in each for every cur row.
    #[cfg(test)]
    fn reference_vote(&mut self, t: usize, class: usize) -> (Vote, Vote) {
        self.voted(t, class);
        let cur_ok = if class == 1 {
            &self.cur.content
        } else {
            &self.cur.anchor
        };
        let mut votes = std::mem::take(&mut self.scratch.votes);
        votes.clear();
        votes.resize(2 * t - 1, (0, 0, 0));
        for (i, &ok) in cur_ok[..t].iter().enumerate() {
            if !ok {
                continue;
            }
            let h = self.cur.hash[i];
            if self.only_cur(h).is_none() {
                continue;
            }
            let Some(p) = self.only_prev(h) else {
                continue;
            };
            if self.prev.text[p] != self.cur.text[i] {
                continue;
            }
            let slot = &mut votes[p + t - 1 - i];
            if slot.0 == 0 {
                slot.1 = i;
            }
            slot.0 += 1;
            slot.2 = i;
        }
        let mut best = Vote::default();
        let mut moved = Vote::default();
        for (k, &(v, top, bot)) in votes.iter().enumerate() {
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
        votes.clear();
        self.scratch.votes = votes;
        (best, moved)
    }

    fn votes_for(&self, row: usize, offset: isize) -> bool {
        if !self.cur.anchor[row] {
            return false;
        }
        let hash = self.cur.hash[row];
        if self.only_cur(hash).is_none() {
            return false;
        }
        let Some(pos) = self.only_prev(hash) else {
            return false;
        };
        pos.cast_signed() - row.cast_signed() == offset && self.prev.text[pos] == self.cur.text[row]
    }

    fn fixed_floor(&self, t: usize, moved: Vote) -> Option<usize> {
        if moved.votes == 0 {
            return None;
        }
        (moved.top..t)
            .find(|&i| self.votes_for(i, 0))
            .filter(|&i| i > moved.bot)
    }

    fn leading_equal(&self, limit: usize) -> usize {
        let mut s0 = 0;
        while s0 < limit && self.prev.hash[s0] == self.cur.hash[s0] {
            s0 += 1;
        }
        s0
    }

    fn pinned_rows_up(&self, top_cur: usize, k: usize) -> usize {
        let s_max = self.leading_equal(top_cur);
        let mut s_min = s_max;
        while s_min > 0 && self.cur.hash[s_min - 1] == self.prev.hash[s_min - 1 + k] {
            s_min -= 1;
        }
        self.pin.clamp(s_min, s_max)
    }

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
            self.below.push_front(HeldRow {
                text: self.prev.text[p].clone(),
                hash: self.prev.hash[p],
                anchor: self.prev.anchor[p],
            });
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

    fn settle_below(&mut self) {
        if self.below.is_empty() {
            return;
        }
        let back = self.below.iter().rposition(|r| {
            r.anchor
                && self
                    .only_cur(r.hash)
                    .is_some_and(|i| self.cur.text[i] == r.text)
        });
        if let Some(m) = back {
            self.below.drain(..=m);
        }
    }

    /// Archives rows `picks` of the previous frame, in order.
    fn archive_prev_rows(&mut self, picks: &[usize]) {
        let Self {
            prev,
            store,
            rows,
            bytes,
            ..
        } = self;
        rows.reserve(picks.len());
        for &i in picks {
            let row = store.push(&prev.text[i], prev.hash[i], prev.anchor[i]);
            *bytes += row.cost();
            rows.push_back(row);
        }
    }

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

    fn reanchor(&mut self, t: usize) -> Option<u64> {
        let len = self.rows.len();
        if len == 0 || t <= self.pin {
            return None;
        }
        let floor = usize::try_from(self.floor.saturating_sub(self.first)).unwrap_or(usize::MAX);
        let lo = len
            .saturating_sub(REANCHOR_SCREENS * self.cur.rows())
            .max(floor);
        // Each cur row unique among the last vote's eligible rows gets the last anchor row of
        // the tail with its hash.
        let mut tail_pos = std::mem::take(&mut self.scratch.tail_pos);
        tail_pos.clear();
        tail_pos.resize(t, NO_ROW);
        #[cfg(test)]
        let reference = self.scratch.reference;
        #[cfg(not(test))]
        let reference = false;
        if reference {
            // The old path: one lookup per tail row in the small cur index.
            for (pos, row) in (lo..).zip(self.rows.range(lo.min(len)..)) {
                if row.anchor
                    && let Some(i) = self.only_cur(row.hash)
                {
                    tail_pos[i] = pos;
                }
            }
        } else if lo < len {
            let from = self.first + lo as u64;
            self.index_tail(from);
            for i in self.pin..t {
                let h = self.cur.hash[i];
                // An anchor is a row that says something, eligible in either class: the only
                // row with its hash is this one or none.
                if self.cur.anchor[i]
                    && self.only_cur_row(i).is_some()
                    && let Some(at) = self.tail.newest.get(h).filter(|&at| at >= from)
                {
                    tail_pos[i] = usize::try_from(at - self.first).unwrap_or(NO_ROW);
                }
            }
        }
        let mut j_votes = std::mem::take(&mut self.scratch.j_votes);
        j_votes.clear();
        for i in self.pin..t {
            if !self.cur.anchor[i] {
                continue;
            }
            if self.only_cur_row(i).is_none() {
                continue;
            }
            let pos = tail_pos[i];
            if pos == NO_ROW {
                continue;
            }
            if self.store.text(&self.rows[pos]) != self.cur.text[i] {
                continue;
            }
            let off = i - self.pin;
            if pos >= off {
                *j_votes.entry((pos - off) as u64, 0).0 += 1;
            }
        }
        self.scratch.tail_pos = tail_pos;
        let at = j_votes
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)))
            .map(|(j, _)| self.first + j);
        self.scratch.j_votes = j_votes;
        at
    }

    /// Brings the tail index up to the archive's end, holding every anchor row from `from` (an
    /// absolute index of a retained row) on. It starts over from `from` when it holds less
    /// than that or has gathered four times more hashes than the rows it must hold.
    fn index_tail(&mut self, from: u64) {
        let end = self.first + self.rows.len() as u64;
        let tail = &mut self.tail;
        let span = usize::try_from(end - from).unwrap_or(usize::MAX);
        if tail.from > from || tail.newest.used > span.saturating_mul(4).saturating_add(64) {
            tail.newest.clear();
            tail.from = from;
            tail.to = from;
        }
        #[cfg(test)]
        if self.scratch.stale_tail && tail.to > tail.from {
            return;
        }
        let start = tail.to.max(self.first);
        let skip = usize::try_from(start - self.first).unwrap_or(usize::MAX);
        // Room for every row it may take first, so no slot moves while they go in.
        tail.newest.reserve(self.rows.len().saturating_sub(skip));
        for (at, row) in (start..).zip(self.rows.iter().skip(skip)) {
            if row.anchor {
                *tail.newest.slot_mut(row.hash, at).1 = at;
            }
        }
        tail.to = end;
    }

    fn archived_hash(&self, index: u64) -> Option<u64> {
        let off = usize::try_from(index.checked_sub(self.first)?).ok()?;
        self.rows.get(off).map(|r| r.hash)
    }

    fn check_debt(&mut self, t: usize) {
        self.debt = self.debt_shown(t, &self.cur.hash);
    }

    /// How many of the debt's rows, from the first, a frame whose rows have hashes `hash` shows
    /// from screen row `pin` on above `t`: the debt it keeps.
    fn debt_shown(&self, t: usize, hash: &[u64]) -> usize {
        if self.debt == 0 {
            return 0;
        }
        let mut n = 0;
        while self.pin + n < t
            && self.archived_hash(self.debt_at + n as u64) == Some(hash[self.pin + n])
        {
            n += 1;
        }
        n
    }

    fn flush_prev_rows(&mut self, from: usize, to: usize) {
        let to = to.min(self.prev.rows());
        let (dlo, dhi) = (self.pin, self.pin + self.debt);
        // The rows leaving: `picks` (prev rows) then all of `below`, blanks cut at both ends.
        let mut picks = std::mem::take(&mut self.scratch.picks);
        picks.clear();
        picks.extend((from..to).filter(|&i| i < dlo || i >= dhi));
        let p = picks.len();
        let blank = |k: usize| {
            if k < p {
                self.prev.blank(picks[k])
            } else {
                self.below[k - p].text.is_empty()
            }
        };
        let mut lo = 0;
        let mut hi = p + self.below.len();
        if self.at_boundary() {
            while lo < hi && blank(lo) {
                lo += 1;
            }
        }
        while hi > lo && blank(hi - 1) {
            hi -= 1;
        }
        self.archive_prev_rows(&picks[lo.min(p)..hi.min(p)]);
        let (blo, bhi) = (lo.max(p) - p, hi.max(p) - p);
        for (k, held) in self.below.drain(..).enumerate() {
            if (blo..bhi).contains(&k) {
                let row = self.store.push(&held.text, held.hash, held.anchor);
                self.bytes += row.cost();
                self.rows.push_back(row);
            }
        }
        self.scratch.picks = picks;
        self.evict();
    }

    fn archive_picks(&mut self) {
        let picks = std::mem::take(&mut self.scratch.picks);
        let mut lo = 0;
        let hi = picks.len();
        if self.at_boundary() {
            while lo < hi && self.prev.blank(picks[lo]) {
                lo += 1;
            }
        }
        self.archive_prev_rows(&picks[lo..hi]);
        self.scratch.picks = picks;
        self.evict();
    }

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
        self.sync_share();
        let budget = self.effective_budget();
        let (mut bytes, mut dropped) = (self.bytes, 0);
        while bytes > budget || self.rows.len() > self.max_rows {
            let Some(row) = self.rows.pop_front() else {
                break;
            };
            bytes -= row.cost();
            dropped += 1;
        }
        self.bytes = bytes;
        self.first += dropped;
        self.lost += dropped;
        if dropped != 0 {
            let keep = self.rows.front().map_or(u64::MAX, |r| r.chunk);
            self.store.release_before(keep);
        }
        self.sync_share();
        while self.gaps.front().is_some_and(|g| g.after + 1 < self.first) {
            self.gaps.pop_front();
        }
        if self.debt > 0 && self.debt_at < self.first {
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

    fn new_run(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.drop_prev();
        self.floor = self.last() + 1;
        self.memo.release();
    }

    fn install_restored(&mut self, cols: u16, key: Option<u64>) {
        if !self.enabled || self.cur.rows() == 0 || self.cur.all_blank() {
            return;
        }
        self.drop_prev();
        self.cur.cols = cols;
        self.install_cur(key);
    }

    /// Discards all retained rows, records a `Reset` gap, and drops the baseline.
    pub fn wipe(&mut self) {
        let n = self.rows.len() as u64;
        self.lost += n;
        self.first += n;
        self.rows.clear();
        self.store.clear();
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

    fn differ_state(&self) -> AltArchiveDiffer {
        AltArchiveDiffer {
            prev: self
                .have_prev
                .then(|| (self.prev.cols, self.prev.text.clone())),
            chrome: self.chrome,
            pin: self.pin,
            debt: self.debt,
            debt_at: self.debt_at,
            below: self.below.iter().map(|r| r.text.clone()).collect(),
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
            .map(|r| Arc::from(self.store.text(r)))
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
        self.store.clear();
        self.tail = TailIndex::default();
        self.bytes = 0;
        let mut normal = String::new();
        for text in carry.rows {
            let text = if is_normalized(&text) {
                &*text
            } else {
                normal.clear();
                normal.push_str(&text);
                normalize_row(&mut normal);
                &normal
            };
            let row = self.store.push(text, row_hash(text), is_anchor_text(text));
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
            .map(|s| HeldRow {
                hash: row_hash(&s),
                anchor: is_anchor_text(&s),
                text: s,
            })
            .collect();
    }
}

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

#[cfg(test)]
fn index_anchors(hash: &[u64], eligible: &[bool], t: usize, idx: &mut HashIndex<(u32, usize)>) {
    idx.clear();
    for i in 0..t {
        if eligible[i] {
            let (e, fresh) = idx.entry(hash[i], (1, i));
            if !fresh {
                e.0 += 1;
            }
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

fn normalize_row(s: &mut String) {
    // Fast screen: C0, DEL, and the UTF-8 lead byte of U+0080..=U+00BF (C1
    // controls live there, alongside common glyphs like `·`), folded over every
    // byte so the scan vectorizes.
    let suspect = s.bytes().fold(0u8, |any, b| {
        any | u8::from(b < 0x20) | u8::from(b == 0x7f) | u8::from(b == 0xc2)
    });
    if suspect != 0 && s.chars().any(|c| c == '\0' || c.is_control()) {
        let mapped: String = s
            .chars()
            .map(|c| if c == '\0' || c.is_control() { ' ' } else { c })
            .collect();
        *s = mapped;
    }
    let spaces = s.bytes().rev().take_while(|&b| b == b' ').count();
    s.truncate(s.len() - spaces);
    let end = s.trim_end().len();
    s.truncate(end);
}

/// The first byte of `hay` that `hit` takes, sixty-four bytes at a time, which vectorizes.
#[inline]
fn find_first(hay: &[u8], hit: impl Fn(u8) -> bool) -> Option<usize> {
    let (blocks, tail) = hay.as_chunks::<64>();
    for (n, block) in blocks.iter().enumerate() {
        let mut any = 0u8;
        for &b in block {
            any |= u8::from(hit(b));
        }
        if any != 0 {
            return block.iter().position(|&b| hit(b)).map(|i| n * 64 + i);
        }
    }
    let base = blocks.len() * 64;
    tail.iter().position(|&b| hit(b)).map(|i| base + i)
}

fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    find_first(hay, |b| b == needle)
}

/// A byte that leaves [`MatchState::Esc`] where it is: a C0 control other than CAN, SUB and
/// ESC (which starts the escape again, so it too leaves the machine in `Esc`).
#[inline]
const fn stays_escaped(b: u8) -> bool {
    matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f)
}

/// A byte that leaves [`MatchState::CsiEntry`] where it is ([`after_csi_entry`]).
#[inline]
const fn stays_in_csi_entry(b: u8) -> bool {
    matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f)
}

/// Whether the machine, in [`MatchState::Ground`] before `bytes[from]`, is in
/// [`MatchState::Esc`] before `bytes[at]`: only an ESC enters that state, from any, and only
/// the bytes [`stays_escaped`] keep it, so it is exactly when the last byte before `at` that
/// does not is an ESC at or after `from`.
#[inline]
fn escaped(bytes: &[u8], from: usize, at: usize) -> bool {
    bytes[from..at].iter().rev().find(|&&b| !stays_escaped(b)) == Some(&0x1b)
}

/// Whether the machine, in [`MatchState::Ground`] before `bytes[from]`, is in
/// [`MatchState::CsiEntry`] before `bytes[at]`: only a `[` in `Esc` enters it, and only the
/// bytes [`stays_in_csi_entry`] keep it.
#[inline]
fn csi_entered(bytes: &[u8], from: usize, at: usize) -> bool {
    bytes[from..at]
        .iter()
        .rposition(|&b| !stays_in_csi_entry(b))
        .is_some_and(|m| bytes[from + m] == b'[' && escaped(bytes, from, from + m))
}

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
    /// A planted divergence for the differential test's negative control: the alt screen's
    /// scan takes no `c` for an RIS.
    #[cfg(test)]
    misses_reset: bool,
}

/// The state after `b` in [`MatchState::Esc`] (an ESC c on the alt screen also hits).
#[inline]
const fn after_esc(b: u8) -> MatchState {
    match b {
        b'[' => MatchState::CsiEntry,
        0x1b => MatchState::Esc,
        b if stays_escaped(b) => MatchState::Esc,
        _ => MatchState::Ground,
    }
}

/// The state after `b` in [`MatchState::CsiEntry`] (parameters reset on `?`).
#[inline]
const fn after_csi_entry(b: u8) -> MatchState {
    match b {
        b'?' => MatchState::Params,
        0x1b => MatchState::Esc,
        b if stays_in_csi_entry(b) => MatchState::CsiEntry,
        _ => MatchState::Ground,
    }
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
        #[cfg(test)]
        let plant = self.misses_reset;
        *self = Self::default();
        #[cfg(test)]
        {
            self.misses_reset = plant;
        }
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
                    #[cfg(test)]
                    let misses_reset = self.misses_reset;
                    #[cfg(not(test))]
                    let misses_reset = false;
                    // Most sequences set no mode, and the machine leaves ground only at an ESC,
                    // so its state at a byte follows from the bytes since it stood here. Only
                    // two bytes can hit or start what does: the `?` of a `CSI ?` and the `c`
                    // of an RIS. The scan goes from one of those to the next and reads back
                    // what the machine would be in there; past the last, where it ends.
                    let from = i;
                    loop {
                        let Some(at) =
                            find_first(&bytes[i..], |b| b == b'?' || b == b'c').map(|k| i + k)
                        else {
                            let end = bytes.len();
                            self.state = if csi_entered(bytes, from, end) {
                                MatchState::CsiEntry
                            } else if escaped(bytes, from, end) {
                                MatchState::Esc
                            } else {
                                MatchState::Ground
                            };
                            return None;
                        };
                        i = at + 1;
                        if bytes[at] == b'c' {
                            if !misses_reset && escaped(bytes, from, at) {
                                return Some(DecModeHit {
                                    final_at: at,
                                    kind: HitKind::Reset,
                                });
                            }
                        } else if csi_entered(bytes, from, at) {
                            self.state = MatchState::Params;
                            self.param = 0;
                            self.esu = false;
                            self.alt = 0;
                            break;
                        }
                    }
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
                    self.state = after_esc(b);
                    if b == b'c' && on_alt {
                        return Some(DecModeHit {
                            final_at: i,
                            kind: HitKind::Reset,
                        });
                    }
                }
                MatchState::CsiEntry => {
                    self.state = after_csi_entry(b);
                    if b == b'?' {
                        self.param = 0;
                        self.esu = false;
                        self.alt = 0;
                    }
                }
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
    pub(super) fn leave_orphaned(&mut self, grid: &mut Grid, sync_end_seq: u64) {
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

    /// Attaches or detaches an isolated shared byte budget for unit tests.
    #[cfg(test)]
    pub(crate) fn set_alt_archive_pool(&mut self, pool: Option<Arc<AltArchiveBudget>>) {
        self.alt_archive.archive.share_budget(pool);
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

    fn xtrestore_leaves_alt(&self, modes: u8) -> bool {
        [(ALT_47, 47u16), (ALT_1047, 1047), (ALT_1049, 1049)]
            .iter()
            .any(|&(bit, mode)| {
                modes & bit != 0 && self.transient.xtsave_modes.get(&mode) == Some(&false)
            })
    }

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
        self.alt_archive.archive.commit_grid(&mut self.grid);
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

    fn alt_archive_install_screen(&mut self) {
        let st = &mut self.alt_archive;
        if !st.archive.enabled || !self.modes.alternate_screen || self.modes.synchronized_output {
            return;
        }
        let grid = &mut self.grid;
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

#[cfg(test)]
#[path = "alt_archive_memo_tests.rs"]
mod memo_tests;
