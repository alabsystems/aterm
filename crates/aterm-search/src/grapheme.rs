// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Minimal grapheme helpers needed by search indexing.

#[cfg(test)]
use aterm_grapheme::grid_graphemes;
use aterm_grapheme::{GraphemeClusters, char_width, grid_graphemes_with};
use aterm_types::text_shaping::AmbiguousWidth;

use crate::cells::{CellSpan, LineCells};

/// Greek small letter final sigma (U+03C2).
const FINAL_SIGMA: char = '\u{03C2}';
/// Greek small letter sigma (U+03C3).
const SIGMA: char = '\u{03C3}';

/// Lowercase a single `char` with the shared context-free routine used by
/// every case-insensitive search site (query key, indexed trigrams, per-line
/// scan buffer, and [`LowerByteMap`]).
///
/// Unlike `str::to_lowercase`, this is purely per-character (no contextual
/// Final_Sigma rule), and it additionally folds Greek final sigma U+03C2 → σ
/// U+03C3 so that capital Σ, medial σ, and word-final ς all collapse to a
/// single canonical form. Without this, `str::to_lowercase` (contextual) and
/// `char::to_lowercase` (non-contextual) disagree on a word-final Σ, so the
/// trigram index, the query key, and the scan buffer end up byte-divergent and
/// case-insensitive search silently misses every sigma match.
///
/// Both ς and σ encode to 2 UTF-8 bytes, so folding never changes a
/// character's lowered byte length — `LowerByteMap` offsets stay aligned.
pub(crate) fn lower_fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase()
        .map(|lc| if lc == FINAL_SIGMA { SIGMA } else { lc })
}

/// How much work lowering `text` actually needs, so callers can skip or
/// specialize the fold instead of always materializing a lowered `String`.
///
/// This is the single home of the predicate the trigram
/// index/removal/rebuild passes share: they MUST agree on when the lowered
/// pass ran, or removal double-deletes trigrams the insert never added and the
/// posting lists corrupt.
pub(crate) enum LowerNeed {
    /// Lowering is the identity (pure ASCII, no uppercase): the lowered
    /// trigrams equal the original-case ones, so the pass can be skipped.
    None,
    /// Pure ASCII with at least one uppercase letter: lowering is exactly
    /// per-byte `to_ascii_lowercase` and never changes byte length, so the
    /// lowered trigram of a window is that window's bytes lowercased in place
    /// — no buffer required.
    Ascii,
    /// Non-ASCII: needs the real [`lower_fold`] (byte lengths can change).
    Unicode,
}

/// Classify `text` for the lowered trigram pass: the two arms of
/// `!text.is_ascii() || text.bytes().any(|b| b.is_ascii_uppercase())`.
pub(crate) fn lower_need(text: &str) -> LowerNeed {
    if !text.is_ascii() {
        LowerNeed::Unicode
    } else if text.bytes().any(|b| b.is_ascii_uppercase()) {
        LowerNeed::Ascii
    } else {
        LowerNeed::None
    }
}

/// Lowercase + sigma-fold `s` into `out` (cleared first), so a caller with a
/// per-line loop can pay for the buffer's capacity ONCE instead of allocating
/// and freeing a fresh `String` per line.
///
/// Shared by the query key and the trigram index/removal/rebuild passes so
/// that index, query, and per-line scan are byte-identical.
pub(crate) fn lower_fold_into(s: &str, out: &mut String) {
    // Spelled as an explicit push loop (not `.collect()`/`extend`) because the
    // Trust L0 gate's bulk-allocation recognizer cannot derive an element count
    // for the flat_map collect and refuses to prove the allocation bounded;
    // incremental `push` growth carries the bounds instead. Identical output.
    out.clear();
    for c in s.chars() {
        for lc in lower_fold_char(c) {
            out.push(lc);
        }
    }
}

/// Lowercase + sigma-fold an entire string using [`lower_fold_char`].
///
/// Thin wrapper over [`lower_fold_into`] so there is one lowering routine.
pub(crate) fn lower_fold(s: &str) -> String {
    let mut out = String::new();
    lower_fold_into(s, &mut out);
    out
}

/// Convert a byte offset to a display column (O(G) per call): the cursor
/// column after the clusters that start before `byte_offset`.
///
/// Reference implementation retained for equivalence testing against
/// [`ColumnMap::end_column`]. Production code uses `ColumnMap` for
/// O(log G) lookups (#5672).
#[cfg(test)]
#[must_use]
pub fn byte_to_column(s: &str, byte_offset: usize) -> usize {
    let mut column: usize = 0;

    for g in grid_graphemes(s) {
        if g.byte_offset >= byte_offset {
            return column;
        }
        column = g.end_col;
    }

    column
}

/// Display columns `text` occupies, in the SAME column frame every
/// [`SearchMatch`](crate::SearchMatch) column is measured in (a wide glyph
/// counts 2, a combining mark 0).
///
/// Exported because the terminal's search caller JOINS a soft-wrapped run of
/// grid rows into the one logical line the user sees before indexing it, and
/// has to pad each continued row back out to the grid's width first. A caller
/// that counted that padding with its OWN width routine would put every column
/// past the first wrap boundary on the wrong cell, so the padding is counted by
/// the engine's counter, not beside it.
#[must_use]
pub fn display_columns(text: &str) -> usize {
    display_columns_with(text, AmbiguousWidth::Single)
}

/// [`display_columns`] under the terminal's `ambiguous_width` policy, the
/// frame a [`SearchIndex`](crate::SearchIndex) built with the same policy
/// measures its matches in.
#[must_use]
pub fn display_columns_with(text: &str, ambiguous: AmbiguousWidth) -> usize {
    // Printable ASCII holds no East Asian Ambiguous character, so the
    // identity holds under either policy.
    if text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
        return text.len();
    }
    // Width-only callers need no byte-offset lookups: the map's same
    // grid walk, without a coordinate entry for every grapheme.
    aterm_grapheme::str_grid_columns_with(text, ambiguous)
}

/// Map a byte offset in lowercased text back to the corresponding byte offset
/// in the original text.
///
/// `to_lowercase()` can change byte lengths (e.g. U+212A Kelvin Sign: 3 bytes
/// → 1-byte 'k'), so searching in the lowered text and using byte offsets
/// directly on the original produces wrong columns. This mapping tracks
/// per-char byte expansion/contraction.
#[cfg(test)]
pub(crate) fn map_lower_byte_to_original(original: &str, lower_byte: usize) -> usize {
    let mut orig_offset = 0usize;
    let mut low_offset = 0usize;

    for ch in original.chars() {
        if low_offset >= lower_byte {
            return orig_offset;
        }
        let orig_len = ch.len_utf8();
        let low_len: usize = ch.to_lowercase().map(|c| c.len_utf8()).sum();
        orig_offset = orig_offset.saturating_add(orig_len);
        low_offset = low_offset.saturating_add(low_len);
    }

    orig_offset
}

/// Precomputed byte-offset-to-column map for a single line.
///
/// Built once per line in O(G) where G is the grapheme count, then enables
/// O(log R) column lookups via binary search over its R runs. This replaces
/// the O(G)-per-call `byte_to_column` pattern that causes O(M × G) total cost
/// when there are M matches in a line.
///
/// Column maps are cached in `SearchIndex` alongside line text so that
/// repeated searches reuse the precomputed map instead of rebuilding it
/// for every query (#7373).
///
/// # Run-length encoded (ruling 2026-10-11, r15 content-3)
///
/// The map is kept for every indexed line, and one entry of three `usize` per
/// grapheme, in a `Vec` grown by doubling, made a non-ASCII line's map 10-25
/// times its text: an index of 100,000 rows of CJK held 370 MiB, of
/// decomposed accents 663 MiB, and one box-drawing glyph in an otherwise ASCII
/// agent row cost 3.4 KB where the plain row cost 389 B. The graphemes of a
/// line repeat one shape (every ideograph 3 bytes and 2 columns, every
/// `e\u{301}` 3 bytes and 1 column), so the map stores RUNS of graphemes
/// that advance by the same bytes and columns, in an exactly sized slice: a
/// row of one script is one run, the agent row two. Decided by measurement
/// against the lazy alternative (build a map per hit line at query time):
/// runs keep every query's lookups where they were, while a lazy map re-walks
/// every hit line on every query. Offsets and columns are `u32`; a line past
/// 4 GiB of text or 4 Gi columns (only a soft-wrapped run of millions of rows
/// joined into one line could be) saturates there, so its columns past that
/// point read as the last one instead of wrapping.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ColumnMap {
    /// The graphemes, in byte order, as runs of a repeating shape. Empty for
    /// the identity map (every byte one grapheme one column wide: printable
    /// ASCII drawn one cell per character).
    runs: Box<[ClusterRun]>,
    /// Length of the source text in bytes: the identity map clamps lookups
    /// here, and a lookup at or past it answers `total`. `u32` like the runs
    /// (a map is one hash-map entry per indexed line, so its size is paid per
    /// line).
    text_len: u32,
    /// The cursor column after the whole line: where a match running to its
    /// end ends.
    total: u32,
    /// What the columns were measured from, so a policy switch re-measures
    /// only what the policy decides.
    source: MapSource,
}

/// Where a [`ColumnMap`]'s columns came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapSource {
    /// The line's text walked by [`grid_graphemes_with`] under this
    /// `ambiguous_width` policy: a caller that hands the index text alone.
    Walk(AmbiguousWidth),
    /// The grid cells the text was read from ([`LineCells`]): the grid
    /// already drew each character at the width it decided, so no policy
    /// moves them.
    Cells,
}

/// Where one grapheme sits on the grid (see [`aterm_grapheme::GridGrapheme`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClusterColumns {
    /// The grapheme's byte offset.
    offset: usize,
    /// The first column it is drawn in: a match starting here starts here.
    start: usize,
    /// The cursor column before it: a match ending here ends here. Larger
    /// than `start` when the grapheme joins the cell before it (the `ය` of
    /// Sinhala `ක්‍ය`, which the grid draws in `ක`'s cell).
    before: usize,
}

/// `count` consecutive graphemes of one shape: grapheme `i` of the run starts
/// at byte `offset + i·bytes` and column `start + i·cols`, and for `i > 0`
/// the cursor before it is that column too (it opens a cell of its own right
/// after the one before). Only the first grapheme may fold into the cell
/// before it (`before > start`) or follow a gap (`before < start`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClusterRun {
    offset: u32,
    start: u32,
    before: u32,
    count: u32,
    /// Bytes from one grapheme of the run to the next (unused when
    /// `count == 1`: the next run's offset bounds a lone grapheme).
    bytes: u16,
    /// Columns from one grapheme of the run to the next (unused when
    /// `count == 1`).
    cols: u16,
}

/// A line offset or column as a run stores it (see [`ColumnMap`] on the
/// 4 GiB saturation).
fn narrow(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl ClusterRun {
    /// Grapheme `i`'s byte offset, start column and cursor-before column.
    fn grapheme(self, i: u32) -> ClusterColumns {
        let offset = (self.offset as usize)
            .saturating_add((i as usize).saturating_mul(usize::from(self.bytes)));
        let start = (self.start as usize)
            .saturating_add((i as usize).saturating_mul(usize::from(self.cols)));
        let before = if i == 0 { self.before as usize } else { start };
        ClusterColumns {
            offset,
            start,
            before,
        }
    }
}

/// Collects a line's graphemes, in byte order, into [`ClusterRun`]s.
struct RunBuilder {
    runs: Vec<ClusterRun>,
}

impl RunBuilder {
    fn new() -> Self {
        Self { runs: Vec::new() }
    }

    fn push(&mut self, g: ClusterColumns) {
        let (offset, start, before) = (narrow(g.offset), narrow(g.start), narrow(g.before));
        if let Some(run) = self.runs.last_mut()
            && start == before
            && run.count < u32::MAX
        {
            if run.count == 1 {
                // The second grapheme fixes the run's shape.
                if let (Some(bytes), Some(cols)) = (
                    offset
                        .checked_sub(run.offset)
                        .and_then(|b| u16::try_from(b).ok()),
                    start
                        .checked_sub(run.start)
                        .and_then(|c| u16::try_from(c).ok()),
                ) && bytes > 0
                {
                    run.bytes = bytes;
                    run.cols = cols;
                    run.count = 2;
                    return;
                }
            } else {
                let next = run.grapheme(run.count);
                if next.offset == offset as usize && next.start == start as usize {
                    run.count = run.count.saturating_add(1);
                    return;
                }
            }
        }
        self.runs.push(ClusterRun {
            offset,
            start,
            before,
            count: 1,
            bytes: 0,
            cols: 0,
        });
    }

    fn finish(self, text_len: usize, total: usize, source: MapSource) -> ColumnMap {
        // Every byte one grapheme one column wide from column 0: the identity,
        // which needs no runs (a control character or a lone wide cell breaks
        // it, so this is printable ASCII in practice).
        let (text_len, total) = (narrow(text_len), narrow(total));
        let identity = text_len > 0
            && total == text_len
            && matches!(
                self.runs.as_slice(),
                [ClusterRun { offset: 0, start: 0, before: 0, count, bytes: 1, cols: 1 }]
                    if *count == text_len
            );
        let runs = if identity {
            Box::default()
        } else {
            self.runs.into_boxed_slice()
        };
        ColumnMap {
            runs,
            text_len,
            total,
            source,
        }
    }
}

impl ColumnMap {
    /// Build a column map from line text, East Asian Ambiguous characters as
    /// wide as the terminal's `ambiguous_width` policy draws them.
    ///
    /// For pure-ASCII text, skips grapheme iteration entirely — O(1)
    /// construction with O(1) lookups via identity mapping (#7375).
    /// For non-ASCII text, builds the full grapheme map in O(G).
    pub fn new(text: &str, ambiguous: AmbiguousWidth) -> Self {
        // Fast path: printable ASCII only — byte offset == display column, under
        // either ambiguous-width policy (ASCII holds no ambiguous character).
        // Control characters such as tab/newline/carriage return can have
        // zero display width, so they must use the full grapheme map.
        if text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
            return Self::identity(text.len(), MapSource::Walk(ambiguous));
        }

        let mut runs = RunBuilder::new();
        let mut before: usize = 0;
        // THE GRID'S OWN WALK, not a second model, and over the whole line:
        // charging each grapheme on its own charged a Devanagari conjunct 2
        // where the grid paints 3, a keycap 1 where it paints 2, and the `ය`
        // the grid folds into the cell before it a cell of its own, so every
        // later column on the row named a cell beside the text.
        for g in grid_graphemes_with(text, ambiguous) {
            runs.push(ClusterColumns {
                offset: g.byte_offset,
                start: g.start_col,
                before,
            });
            before = g.end_col;
        }
        runs.finish(text.len(), before, MapSource::Walk(ambiguous))
    }

    /// Build a column map from line text and the grid cells it was read
    /// from: each grapheme is drawn in the cell that holds its first byte,
    /// at the column the grid put that cell.
    ///
    /// Graphemes, not cells, are what a match starts on (a match starting
    /// inside one starts at the next, as [`ColumnMap::new`]'s do), and the
    /// columns come from the cells:
    ///
    /// * a grapheme that starts a cell is drawn from that cell's column, and
    ///   the cursor before it is where the previous cell ends;
    /// * one that starts inside a cell was folded into it: a printing
    ///   character (the `ය` of Sinhala `ක්‍ය`) is drawn in that cell, a
    ///   zero-width one (a ZWSP stored after a letter) in none, at the
    ///   column after it;
    /// * bytes no cell holds (a mark the grid had no cell to store in, text
    ///   past the row's last cell) are drawn in none, after the cell before
    ///   them.
    ///
    /// Written left to right in one go, a line's cells are what
    /// [`ColumnMap::new`] walks its text into, and the two maps agree; where
    /// the grid was written otherwise, only this one names the cells on the
    /// glass.
    pub fn with_cells(text: &str, cells: &LineCells) -> Self {
        // Fast path: every byte read as a one-column cell of its own from
        // column 0 (a row of printable ASCII, which is every ordinary row) is
        // the identity, as the cells say.
        if cells.is_identity(text.len()) {
            return Self::identity(text.len(), MapSource::Cells);
        }
        let mut runs = RunBuilder::new();
        let mut spans = cells
            .iter()
            .take_while(|cell| cell.byte < text.len())
            .peekable();
        let mut previous: Option<CellSpan> = None;
        let mut current: Option<CellSpan> = None;
        for (offset, grapheme) in text.grapheme_indices() {
            while let Some(cell) = spans.next_if(|cell| cell.byte <= offset) {
                previous = current.replace(cell);
            }
            let (start, before) = match current {
                // Before any cell.
                None => (0, 0),
                Some(cell) if offset == cell.byte => {
                    (cell.col, previous.map_or(0, |cell| cell.end_col))
                }
                Some(cell) if offset < cell.end => {
                    let folded = grapheme.chars().next().is_some_and(|c| char_width(c) > 0);
                    if folded {
                        (cell.col, cell.end_col)
                    } else {
                        (cell.end_col, cell.end_col)
                    }
                }
                // Past the cell's text, before the next cell's.
                Some(cell) => (cell.end_col, cell.end_col),
            };
            runs.push(ClusterColumns {
                offset,
                start,
                before,
            });
        }
        // The cells left unread start past every grapheme; the last cell is
        // the line's end.
        let last = spans.last().or(current);
        let total = last.map_or(0, |cell| cell.end_col);
        runs.finish(text.len(), total, MapSource::Cells)
    }

    /// The identity map over `len` bytes: every byte one grapheme one column
    /// wide, which needs no runs.
    fn identity(len: usize, source: MapSource) -> Self {
        Self {
            runs: Box::default(),
            text_len: narrow(len),
            total: narrow(len),
            source,
        }
    }

    /// What this map's columns were measured from.
    #[must_use]
    pub(crate) fn source(&self) -> MapSource {
        self.source
    }

    /// The columns a match over bytes `start..end` is drawn in, as
    /// `(start_col, end_col)` with `end_col` exclusive.
    ///
    /// O(1) for ASCII text (identity mapping), O(log R) otherwise. A byte
    /// offset inside a grapheme maps to the grapheme boundary after it.
    #[must_use]
    #[inline]
    pub fn span(&self, start: usize, end: usize) -> (usize, usize) {
        if self.runs.is_empty() {
            // The identity: byte offset == column, clamped at the line's end.
            let len = self.text_len as usize;
            return (start.min(len), end.min(len));
        }
        (self.start_column(start), self.end_column(end))
    }

    /// The column a match starting at `byte_offset` starts in: the first
    /// column of the grapheme there, which is the cell before it when the
    /// grid folds the grapheme into that cell.
    #[must_use]
    #[inline]
    fn start_column(&self, byte_offset: usize) -> usize {
        self.lookup(byte_offset).start
    }

    /// The column a match ending at `byte_offset` ends at (exclusive): the
    /// cursor column after every grapheme that starts before it.
    #[must_use]
    #[inline]
    fn end_column(&self, byte_offset: usize) -> usize {
        self.lookup(byte_offset).before
    }

    /// The grapheme at `byte_offset`, or the first one after it when the
    /// offset falls inside one; past the last grapheme, the line's end.
    #[inline]
    fn lookup(&self, byte_offset: usize) -> ClusterColumns {
        let (text_len, total) = (self.text_len as usize, self.total as usize);
        let end = ClusterColumns {
            offset: text_len,
            start: total,
            before: total,
        };
        if byte_offset >= text_len {
            return end;
        }
        if self.runs.is_empty() {
            // ASCII fast path: byte offset == column.
            return ClusterColumns {
                offset: byte_offset,
                start: byte_offset,
                before: byte_offset,
            };
        }
        // The last run starting at or before the offset (the first run starts
        // at byte 0, so there is one for any offset inside the text).
        let idx = self
            .runs
            .partition_point(|run| run.offset as usize <= byte_offset)
            .saturating_sub(1);
        let Some(&run) = self.runs.get(idx) else {
            return end;
        };
        let into = byte_offset.saturating_sub(run.offset as usize);
        // A divisor the type proves nonzero: a run of more than one grapheme
        // always has a stride.
        let (i, inside) = match std::num::NonZeroUsize::new(usize::from(run.bytes)) {
            Some(bytes) if run.count > 1 => (into / bytes, into % bytes != 0),
            _ => (0, into != 0),
        };
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if !inside && i < run.count {
            return run.grapheme(i);
        }
        // Inside grapheme `i`: the next one, in this run or the next.
        let next = i.saturating_add(1);
        if next < run.count {
            return run.grapheme(next);
        }
        self.runs
            .get(idx.saturating_add(1))
            .map_or(end, |run| run.grapheme(0))
    }

    /// Heap bytes this map holds (its runs), for the index's accounting tests.
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.runs
            .len()
            .saturating_mul(std::mem::size_of::<ClusterRun>())
    }
}

/// Precomputed lowered-byte-to-original-byte map for case-insensitive search.
///
/// Built once per line in O(C) where C is the char count, then enables
/// O(log C) lookups via binary search. This replaces the O(C)-per-call
/// `map_lower_byte_to_original` pattern that causes O(M × C) total cost
/// when there are M case-insensitive matches in a line.
pub(crate) struct LowerByteMap {
    /// (lower_byte_offset, original_byte_offset) pairs at char boundaries,
    /// sorted by lower_byte_offset. Includes a sentinel at end.
    entries: Vec<(usize, usize)>,
}

impl LowerByteMap {
    /// Build a lower-byte map from original text. O(C) where C = char count.
    pub fn new(original: &str) -> Self {
        let mut entries = Vec::new();
        let mut orig_offset: usize = 0;
        let mut low_offset: usize = 0;
        for ch in original.chars() {
            entries.push((low_offset, orig_offset));
            let orig_len = ch.len_utf8();
            // Use the same lower+sigma-fold routine as the scan buffer so the
            // lowered byte lengths stay byte-identical (ς/σ are both 2 bytes,
            // so the fold never changes a length, but this keeps the mapping
            // tied to the one canonical lowering routine).
            let low_len: usize = lower_fold_char(ch).map(|c| c.len_utf8()).sum();
            orig_offset = orig_offset.saturating_add(orig_len);
            low_offset = low_offset.saturating_add(low_len);
        }
        // Sentinel so lookups at total lowered length return total original length.
        entries.push((low_offset, orig_offset));
        Self { entries }
    }

    /// Map a byte offset in lowered text to the corresponding byte offset in
    /// original text. O(log C). Semantics match `map_lower_byte_to_original`.
    #[must_use]
    pub fn map_to_original(&self, lower_byte: usize) -> usize {
        let total = self.entries.last().map_or(0, |&(_, orig)| orig);
        match self
            .entries
            .binary_search_by_key(&lower_byte, |&(lo, _)| lo)
        {
            Ok(idx) => self.entries.get(idx).map_or(total, |&(_, orig)| orig),
            Err(idx) if idx < self.entries.len() => {
                self.entries.get(idx).map_or(total, |&(_, orig)| orig)
            }
            Err(_) => total,
        }
    }
}

#[cfg(test)]
mod tests {
    use aterm_types::text_shaping::AmbiguousWidth;

    use aterm_grapheme::{GridColumns, GridStep, grid_graphemes_with};

    use super::{
        ColumnMap, LowerByteMap, byte_to_column, display_columns, map_lower_byte_to_original,
    };
    use crate::cells::LineCells;

    /// The per-grapheme map the runs encode, built and looked up the way the
    /// map did before it was run-length encoded (one `(offset, start,
    /// before)` entry per grapheme plus a sentinel, binary-searched): the
    /// oracle the encoding must answer identically to.
    struct ReferenceMap(Vec<(usize, usize, usize)>);

    impl ReferenceMap {
        fn new(text: &str, ambiguous: AmbiguousWidth) -> Self {
            let mut entries = Vec::new();
            let mut before = 0;
            for g in grid_graphemes_with(text, ambiguous) {
                entries.push((g.byte_offset, g.start_col, before));
                before = g.end_col;
            }
            entries.push((text.len(), before, before));
            Self(entries)
        }

        fn look(&self, offset: usize, pick: fn(&(usize, usize, usize)) -> usize) -> usize {
            let total = self.0.last().map_or(0, |e| e.2);
            match self.0.binary_search_by_key(&offset, |e| e.0) {
                Ok(i) => pick(&self.0[i]),
                Err(i) if i < self.0.len() => pick(&self.0[i]),
                Err(_) => total,
            }
        }

        fn span(&self, start: usize, end: usize) -> (usize, usize) {
            (self.look(start, |e| e.1), self.look(end, |e| e.2))
        }
    }

    /// Every piece of a line that changes how its graphemes advance: ASCII,
    /// controls, wide, combining, folding, presentation selectors, flags,
    /// conjuncts, ambiguous-width symbols.
    const SHAPES: &[&str] = &[
        "",
        "ab",
        "\t",
        "日本",
        "e\u{301}",
        "\u{301}",
        "न्दी",
        "\u{d9a}\u{dca}\u{200d}\u{dba}",
        "1\u{fe0f}\u{20e3}",
        "👩🏽‍💻",
        "\u{1f1e6}\u{1f1e7}",
        "\u{1f1e6}",
        "⌚\u{fe0e}",
        "♥\u{fe0f}",
        "→─°",
        "\u{23bf}",
        "\u{200b}",
    ];

    /// The run-length encoded map answers every `(start, end)` byte pair
    /// exactly as the per-grapheme map it replaced, under both
    /// `ambiguous_width` policies: on every pair and triple of shapes, and on
    /// long lines where a shape repeats (one run) and then changes.
    #[test]
    fn the_runs_answer_as_the_per_grapheme_map_did() {
        let mut texts: Vec<String> = Vec::new();
        for a in SHAPES {
            for b in SHAPES {
                texts.push(format!("{a}{b}"));
                for c in SHAPES {
                    texts.push(format!("{a}{b}{c}"));
                }
            }
        }
        for shape in SHAPES {
            texts.push(format!("{}x{}", shape.repeat(9), shape.repeat(3)));
        }
        for ambiguous in [AmbiguousWidth::Single, AmbiguousWidth::Double] {
            for text in &texts {
                let map = ColumnMap::new(text, ambiguous);
                let reference = ReferenceMap::new(text, ambiguous);
                for start in 0..=text.len() + 1 {
                    for end in start..=text.len() + 1 {
                        assert_eq!(
                            map.span(start, end),
                            reference.span(start, end),
                            "{text:?} {ambiguous:?} bytes {start}..{end}"
                        );
                    }
                }
            }
        }
    }

    /// The cells a grid writing `text` left to right in one go stores it in,
    /// as [`GridColumns`] lays them out: a printing character opens a cell,
    /// a folded or zero-width one joins the cell before it (VS16 widening
    /// it), and one with no cell before it is stored nowhere.
    fn written_cells(text: &str, ambiguous: AmbiguousWidth) -> LineCells {
        let mut grid = GridColumns::with_ambiguous_width(ambiguous);
        let mut cells = LineCells::new();
        let mut open: Option<(usize, usize, usize, usize)> = None;
        for (byte, c) in text.char_indices() {
            let end = byte + c.len_utf8();
            match grid.step(c) {
                GridStep::Opens { col, width } => {
                    if let Some((start, stop, col, width)) = open.take() {
                        cells.push(start..stop, col, width);
                    }
                    open = Some((byte, end, col, width));
                }
                GridStep::Folds { .. } | GridStep::Joins { .. } => {
                    if let Some(cell) = open.as_mut() {
                        cell.1 = end;
                        cell.3 = grid.column() - cell.2;
                    }
                }
            }
        }
        if let Some((start, stop, col, width)) = open {
            cells.push(start..stop, col, width);
        }
        cells
    }

    /// Read from cells laid out as a line written left to right in one go,
    /// the map answers every byte pair as the walk of the text does: the
    /// cells only change an answer where the grid did something else. (VS15
    /// on a wide glyph is left out: the walk keeps the cursor past the
    /// emptied column with no cell there, where the grid stores an empty
    /// cell, which its reader reads as a blank.)
    #[test]
    fn cells_of_a_line_written_in_one_go_answer_as_the_walk() {
        let shapes: Vec<&str> = SHAPES
            .iter()
            .copied()
            .filter(|shape| !shape.contains('\u{fe0e}'))
            .collect();
        for ambiguous in [AmbiguousWidth::Single, AmbiguousWidth::Double] {
            for a in &shapes {
                for b in &shapes {
                    for c in &shapes {
                        let text = format!("{a}{b}{c}");
                        let walked = ColumnMap::new(&text, ambiguous);
                        let celled = ColumnMap::with_cells(&text, &written_cells(&text, ambiguous));
                        for start in 0..=text.len() + 1 {
                            for end in start..=text.len() + 1 {
                                assert_eq!(
                                    celled.span(start, end),
                                    walked.span(start, end),
                                    "{text:?} {ambiguous:?} bytes {start}..{end}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Where the grid kept apart what a walk of the text folds, the cells
    /// decide: two flag halves in cells 0-1 and 2-3 put `XYZ` in cells 4-6
    /// (the walk pairs them into one flag and says 2-4); a skin tone in a
    /// cell of its own after its base does the same; a base in a row's last
    /// column left narrow by its VS16 keeps the next cell where the grid has
    /// it.
    #[test]
    fn cells_the_grid_kept_apart_are_where_matches_are_drawn() {
        let flag_halves = "\u{1f1e6}\u{1f1e7}XYZ";
        let mut cells = LineCells::new();
        cells.push(0..4, 0, 2);
        cells.push(4..8, 2, 2);
        for i in 0..3 {
            cells.push(8 + i..9 + i, 4 + i, 1);
        }
        let map = ColumnMap::with_cells(flag_halves, &cells);
        assert_eq!(map.span(8, 11), (4, 7));
        assert_eq!(
            map.span(0, 4),
            (0, 4),
            "the pair is one grapheme, over both cells"
        );
        assert_eq!(
            ColumnMap::new(flag_halves, AmbiguousWidth::Single).span(8, 11),
            (2, 5),
            "precondition: the walk pairs the halves"
        );

        let base_last = "a\u{2714}\u{fe0f}b";
        let mut cells = LineCells::new();
        cells.push(0..1, 0, 1);
        cells.push(1..7, 1, 1);
        cells.push(7..8, 2, 1);
        let map = ColumnMap::with_cells(base_last, &cells);
        assert_eq!(map.span(7, 8), (2, 3));
        assert_eq!(map.span(0, 8), (0, 3), "the line is three cells");
    }

    /// A grapheme inside a cell is drawn in it when it prints (the grid
    /// folded it there) and in no column when it is zero-width, and bytes
    /// no cell holds are drawn in none: the rules that keep a lone mark, a
    /// ZWSP or a prepended digit out of the results.
    #[test]
    fn graphemes_inside_a_cell_and_bytes_outside_every_cell() {
        // `ක්‍` and `ය` in one cell at column 2.
        let yansaya = "x \u{d9a}\u{dca}\u{200d}\u{dba} y";
        let mut cells = LineCells::new();
        cells.push(0..1, 0, 1);
        cells.push(1..2, 1, 1);
        cells.push(2..14, 2, 1);
        cells.push(14..15, 3, 1);
        cells.push(15..16, 4, 1);
        let map = ColumnMap::with_cells(yansaya, &cells);
        assert_eq!(map.span(11, 14), (2, 3), "the folded letter is in its cell");

        let zwsp = "a\u{200b}b";
        let mut cells = LineCells::new();
        cells.push(0..4, 0, 1);
        cells.push(4..5, 1, 1);
        let map = ColumnMap::with_cells(zwsp, &cells);
        assert_eq!(map.span(1, 4), (1, 1), "a ZWSP is drawn in no column");

        // A mark with no cell to land in, then text past the last cell.
        let orphans = "\u{301}ab\u{301}";
        let mut cells = LineCells::new();
        cells.push(2..3, 0, 1);
        cells.push(3..4, 1, 1);
        let map = ColumnMap::with_cells(orphans, &cells);
        assert_eq!(map.span(0, 2), (0, 0));
        assert_eq!(map.span(2, 4), (0, 2));
        assert_eq!(map.span(4, 6), (2, 2));
    }

    /// A map is one hash-map entry per indexed line, ASCII lines included:
    /// it stays the 32 bytes the per-grapheme map took.
    #[test]
    fn a_map_is_no_larger_than_before() {
        assert!(std::mem::size_of::<ColumnMap>() <= 32);
    }

    /// A line of one shape is one run however long it is, and the runs are
    /// sized exactly: the map of a 100-ideograph row, of 100 decomposed
    /// accents, or of an ASCII row with one box-drawing glyph holds a few
    /// dozen bytes, not one 24-byte entry per grapheme with doubling slack.
    #[test]
    fn a_line_of_one_shape_is_one_run() {
        let one = |text: &str| ColumnMap::new(text, AmbiguousWidth::Single).heap_bytes();
        let run = one("日日");
        assert!(run > 0);
        assert_eq!(one(&"日".repeat(100)), run, "CJK");
        assert_eq!(one(&"e\u{301}".repeat(100)), run, "decomposed accents");
        assert_eq!(
            one(&format!("  \u{23bf}  {}", "x".repeat(110))),
            2 * run,
            "ASCII ending in the glyph, then ASCII"
        );
        assert_eq!(
            one(&"plain ascii ".repeat(10)),
            0,
            "the identity holds nothing"
        );
    }

    #[test]
    fn width_only_matches_coordinate_map_for_unicode_and_controls() {
        let pieces = [
            "",
            "plain ASCII ~",
            "\0\t\r\n\u{7f}",
            "日本語",
            "e\u{0301}",
            "\u{0301}\u{0308}",
            "\u{fe0f}",
            "न्दी",
            "1\u{fe0f}\u{20e3}",
            "👩🏽‍💻",
            "🇦",
            "🇧",
            "♥\u{fe0e}",
            "♥\u{fe0f}",
        ];
        // Pairing includes boundaries where a combining mark, variation
        // selector or regional indicator joins the preceding piece.
        for left in pieces {
            for right in pieces {
                let text = format!("{left}{right}");
                assert_eq!(
                    display_columns(&text),
                    ColumnMap::new(&text, AmbiguousWidth::Single).end_column(text.len()),
                    "text={text:?}"
                );
            }
        }
        // These widths distinguish terminal cells from byte, scalar and
        // grapheme counts; controls must not enter the ASCII identity path.
        assert_eq!(display_columns("\0\t\r\n\u{7f}"), 0);
        assert_eq!(display_columns("日"), 2);
        assert_eq!(display_columns("न्दी"), 3);
        assert_eq!(display_columns("1\u{fe0f}\u{20e3}"), 2);
        assert_eq!(display_columns("👩🏽‍💻"), 2);
    }

    /// ColumnMap produces identical results to byte_to_column for ASCII text.
    #[test]
    fn column_map_matches_byte_to_column_ascii() {
        let text = "hello world, this is a test line!";
        let map = ColumnMap::new(text, AmbiguousWidth::Single);
        for offset in 0..=text.len() {
            assert_eq!(
                map.end_column(offset),
                byte_to_column(text, offset),
                "mismatch at offset {offset}"
            );
        }
    }

    /// ColumnMap matches byte_to_column for wide CJK characters.
    #[test]
    fn column_map_matches_byte_to_column_cjk() {
        let text = "ab日本語cd";
        let map = ColumnMap::new(text, AmbiguousWidth::Single);
        for offset in 0..=text.len() {
            assert_eq!(
                map.end_column(offset),
                byte_to_column(text, offset),
                "mismatch at offset {offset}"
            );
        }
    }

    /// ColumnMap handles empty strings.
    #[test]
    fn column_map_empty_string() {
        let map = ColumnMap::new("", AmbiguousWidth::Single);
        assert_eq!(map.span(0, 0), (0, 0));
    }

    /// ColumnMap equivalence across mixed-width Unicode: combining characters,
    /// emoji, CJK, and ASCII in a single line.
    #[test]
    fn column_map_matches_byte_to_column_mixed_unicode() {
        let texts = [
            "abc\u{0301}def",          // combining acute accent on 'c'
            "\u{1F600}hello\u{1F600}", // emoji + ASCII + emoji
            "日a本b語c",               // interleaved CJK and ASCII
            "a\u{0308}\u{0301}b",      // stacked combining marks
            "\t\n\r abc",              // control chars + ASCII
        ];
        for text in &texts {
            let map = ColumnMap::new(text, AmbiguousWidth::Single);
            for offset in 0..=text.len() {
                assert_eq!(
                    map.end_column(offset),
                    byte_to_column(text, offset),
                    "mismatch in {text:?} at offset {offset}"
                );
            }
        }
    }

    /// Stress test: ColumnMap is equivalent to byte_to_column on a long line
    /// with many match positions. This validates the O(log G) lookup path
    /// produces identical results to the O(G) scan for all byte offsets.
    #[test]
    fn column_map_long_line_all_offsets() {
        // 80 CJK chars = 240 bytes, 160 display columns
        let text: String = "日本語テスト漢字表示".chars().cycle().take(80).collect();
        let map = ColumnMap::new(&text, AmbiguousWidth::Single);
        for offset in 0..=text.len() {
            assert_eq!(
                map.end_column(offset),
                byte_to_column(&text, offset),
                "mismatch at byte offset {offset} in {}-byte CJK line",
                text.len()
            );
        }
    }

    /// LowerByteMap produces identical results to map_lower_byte_to_original
    /// for pure ASCII text where lowering preserves byte lengths.
    #[test]
    fn lower_byte_map_matches_ascii() {
        let text = "Hello World Test";
        let lowered = text.to_lowercase();
        let map = LowerByteMap::new(text);
        for offset in 0..=lowered.len() {
            assert_eq!(
                map.map_to_original(offset),
                map_lower_byte_to_original(text, offset),
                "mismatch at lower offset {offset}"
            );
        }
    }

    /// LowerByteMap handles the Kelvin sign (U+212A) which lowercases from
    /// 3 bytes to 1 byte, causing byte-length divergence.
    #[test]
    fn lower_byte_map_matches_kelvin_sign() {
        // U+212A KELVIN SIGN (3 bytes) → 'k' (1 byte)
        let text = "a\u{212A}b";
        let lowered = text.to_lowercase();
        let map = LowerByteMap::new(text);
        for offset in 0..=lowered.len() {
            assert_eq!(
                map.map_to_original(offset),
                map_lower_byte_to_original(text, offset),
                "mismatch at lower offset {offset} for Kelvin sign text"
            );
        }
    }

    /// LowerByteMap matches for mixed-width Unicode including CJK and emoji.
    #[test]
    fn lower_byte_map_matches_mixed_unicode() {
        let texts = [
            "ABCdef",    // simple mixed case
            "Straße",    // ß stays 2 bytes lowered
            "ΔΕΛΤΑ",     // Greek uppercase → lowercase
            "日本語ABC", // CJK + ASCII
            "",          // empty
        ];
        for text in &texts {
            let lowered = text.to_lowercase();
            let map = LowerByteMap::new(text);
            for offset in 0..=lowered.len() {
                assert_eq!(
                    map.map_to_original(offset),
                    map_lower_byte_to_original(text, offset),
                    "mismatch in {text:?} at lower offset {offset}"
                );
            }
        }
    }

    /// LowerByteMap stress test on a long line with many char boundaries.
    #[test]
    fn lower_byte_map_long_line_all_offsets() {
        let text: String = "Hello日本語World".chars().cycle().take(100).collect();
        let lowered = text.to_lowercase();
        let map = LowerByteMap::new(&text);
        for offset in 0..=lowered.len() {
            assert_eq!(
                map.map_to_original(offset),
                map_lower_byte_to_original(&text, offset),
                "mismatch at lower offset {offset} in {}-byte line",
                text.len()
            );
        }
    }
}
