// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme width, case-folding, and byte-to-column mapping for search indexing.

#[cfg(test)]
use aterm_grapheme::grid_graphemes;
use aterm_grapheme::{GraphemeClusters, char_width, grid_graphemes_with};
use aterm_types::text_shaping::AmbiguousWidth;

use crate::cells::{CellSpan, LineCells};

/// Greek small letter final sigma (U+03C2).
const FINAL_SIGMA: char = '\u{03C2}';
/// Greek small letter sigma (U+03C3).
const SIGMA: char = '\u{03C3}';

/// Lowercase one `char`, folding Greek final sigma (`ς`) to standard sigma (`σ`).
#[inline]
pub(crate) fn lower_fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase()
        .map(|lc| if lc == FINAL_SIGMA { SIGMA } else { lc })
}

/// Work required to lowercase `text` for case-insensitive trigram indexing.
pub(crate) enum LowerNeed {
    /// Pure ASCII with no uppercase letters.
    None,
    /// Pure ASCII with at least one uppercase letter.
    Ascii,
    /// Non-ASCII text requiring full [`lower_fold`].
    Unicode,
}

/// Classify `text` for the lowered trigram pass.
#[inline]
pub(crate) fn lower_need(text: &str) -> LowerNeed {
    if !text.is_ascii() {
        LowerNeed::Unicode
    } else if text.bytes().any(|b| b.is_ascii_uppercase()) {
        LowerNeed::Ascii
    } else {
        LowerNeed::None
    }
}

/// Lowercase and sigma-fold `s` into `out` (cleared first).
pub(crate) fn lower_fold_into(s: &str, out: &mut String) {
    out.clear();
    for c in s.chars() {
        for lc in lower_fold_char(c) {
            out.push(lc);
        }
    }
}

/// Lowercase and sigma-fold `s`.
pub(crate) fn lower_fold(s: &str) -> String {
    let mut out = String::new();
    lower_fold_into(s, &mut out);
    out
}

/// Convert a byte offset in `s` to a display column in O(G).
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

/// Display columns `text` occupies under `AmbiguousWidth::Narrow`.
#[inline]
#[must_use]
pub fn display_columns(text: &str) -> usize {
    display_columns_with(text, AmbiguousWidth::Single)
}

/// Display columns `text` occupies under the given `ambiguous` width policy.
#[inline]
#[must_use]
pub fn display_columns_with(text: &str, ambiguous: AmbiguousWidth) -> usize {
    // Printable ASCII holds no East Asian Ambiguous character, so the
    // identity holds under either policy.
    if printable_ascii(text.as_bytes()) {
        return text.len();
    }
    aterm_grapheme::str_grid_columns_with(text, ambiguous)
}

/// Whether every byte is in `0x20..=0x7e`, using safe eight-byte array reads.
fn printable_ascii(bytes: &[u8]) -> bool {
    let (words, tail) = bytes.as_chunks::<8>();
    for word in words {
        if !printable_word(word) {
            return false;
        }
    }
    tail.iter().all(|byte| matches!(byte, b' '..=b'~'))
}

#[inline(always)]
fn printable_word(bytes: &[u8; 8]) -> bool {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let word = u64::from_ne_bytes(*bytes);
    if word & HIGH != 0 {
        return false;
    }
    // Each lane is now <= 0x7f, so neither addition carries into another lane.
    // Adding 0x60 sets the high bit at 0x20; adding 1 sets it only at DEL.
    word.wrapping_add(0x6060_6060_6060_6060) & HIGH == HIGH
        && word.wrapping_add(0x0101_0101_0101_0101) & HIGH == 0
}

/// Map a byte offset in lowercased text back to `original` in O(C).
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

/// Run-length encoded byte-offset-to-column map for one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColumnMap {
    /// The graphemes, in byte order, as runs of a repeating shape; empty for the identity map.
    runs: Box<[ClusterRun]>,
    /// Length of the source text in bytes: the identity map clamps lookups
    /// here, and a lookup at or past it answers `total`.
    text_len: u32,
    /// The cursor column after the whole line: where a match running to its
    /// end ends.
    total: u32,
    /// What the columns were measured from, so a policy switch re-measures
    /// only what the policy decides.
    source: MapSource,
    /// Whether the source text is ASCII, cached while its columns are built.
    ascii: bool,
}

const _: () = assert!(std::mem::size_of::<ColumnMap>() <= 32);

/// Columns already measured by the index, or measured only after a row matches.
pub(crate) trait SearchColumns {
    /// Source-byte classification, independent of the stored cells' widths.
    fn is_ascii(&self) -> bool;
    /// Resolve the actual columns, retaining stored-cell semantics.
    fn get(&self) -> &ColumnMap;
}

impl SearchColumns for ColumnMap {
    #[inline]
    fn is_ascii(&self) -> bool {
        self.is_ascii()
    }

    #[inline]
    fn get(&self) -> &ColumnMap {
        self
    }
}

/// Source from which a [`ColumnMap`]'s columns were measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapSource {
    /// Walked from line text under this `AmbiguousWidth` policy.
    Walk(AmbiguousWidth),
    /// Measured directly from [`LineCells`].
    Cells,
}

/// Where one grapheme sits on the grid (see [`aterm_grapheme::GridGrapheme`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClusterColumns {
    /// The grapheme's byte offset.
    offset: usize,
    /// The first column it is drawn in: a match starting here starts here.
    start: usize,
    /// The cursor column before it: a match ending here ends here.
    before: usize,
}

/// Coordinates of one grapheme boundary in its retained cells.
#[inline(always)]
fn stored_coordinates(
    offset: usize,
    grapheme: &str,
    current: Option<CellSpan>,
    previous: usize,
) -> (usize, usize) {
    match current {
        None => (0, 0),
        Some(cell) if offset == cell.byte => (cell.col, previous),
        Some(cell) if offset < cell.end => {
            let folded = grapheme.chars().next().is_some_and(|c| char_width(c) > 0);
            if folded {
                (cell.col, cell.end_col)
            } else {
                (cell.end_col, cell.end_col)
            }
        }
        Some(cell) => (cell.end_col, cell.end_col),
    }
}

/// Project one stored-cell span, walking only through its furthest endpoint.
#[inline]
pub(crate) fn stored_span(
    text: &str,
    cells: &LineCells,
    start: usize,
    end: usize,
) -> (usize, usize) {
    let len = narrow(text.len()) as usize;
    if cells.is_identity(text.len()) {
        return (start.min(len), end.min(len));
    }
    projected_stored_span(text, cells, start, end, len)
}

/// Two printable ASCII characters break regardless of earlier Unicode context.
/// Retaining the second character also resets the iterator's contextual state.
#[inline]
fn local_ascii_boundary(bytes: &[u8], at: usize) -> bool {
    matches!(bytes[at], b' '..=b'~') && (at == 0 || matches!(bytes[at - 1], b' '..=b'~'))
}

// Keep the run/grapheme cursors' frame off direct identity lookups.
#[inline(never)]
fn projected_stored_span(
    text: &str,
    cells: &LineCells,
    start: usize,
    end: usize,
    len: usize,
) -> (usize, usize) {
    if start >= len && end >= len {
        let (cell, _) = cells.cell_and_previous(usize::MAX, text.len());
        let total = narrow(cell.map_or(0, |cell| cell.end_col)) as usize;
        return (total, total);
    }
    let mut retained: (Option<CellSpan>, usize) = (None, 0);
    let bytes = text.as_bytes();
    let mut base = 0;
    let mut columns = text.grapheme_indices();
    let mut current = None;
    let mut started = false;
    let mut anchor = 0;
    let mut lookup = |byte| {
        if byte < len {
            let boundary = if local_ascii_boundary(bytes, byte) {
                anchor = byte;
                Some((byte, &text[byte..byte + 1]))
            } else {
                // A UTF-8 character starts at most three bytes before an interior
                // byte. Keep a proven ASCII predecessor for following extenders.
                let mut char_start = byte;
                while bytes[char_start] & 0xc0 == 0x80 {
                    char_start -= 1;
                }
                if char_start > 0 && local_ascii_boundary(bytes, char_start - 1) {
                    anchor = anchor.max(char_start - 1);
                }
                if !started || anchor > base {
                    base = anchor;
                    columns = text[base..].grapheme_indices();
                    current = columns.next();
                    started = true;
                }
                let relative = byte - base;
                while current.is_some_and(|(offset, _)| offset < relative) {
                    current = columns.next();
                }
                current.map(|(offset, grapheme)| (base + offset, grapheme))
            };
            if let Some((offset, grapheme)) = boundary {
                if !retained
                    .0
                    .is_some_and(|cell| offset >= cell.byte && offset < cell.end)
                {
                    retained = cells.cell_and_previous(offset, text.len());
                }
                let (cell, previous) = retained;
                let (start, before) = stored_coordinates(offset, grapheme, cell, previous);
                return ClusterColumns {
                    offset,
                    start,
                    before,
                };
            }
        }
        let (cell, _) = cells.cell_and_previous(usize::MAX, text.len());
        let total = cell.map_or(0, |cell| cell.end_col);
        ClusterColumns {
            offset: text.len(),
            start: total,
            before: total,
        }
    };
    // The public helper also accepts reversed offsets. Walk in byte order,
    // retaining each endpoint's distinct start/cursor-before interpretation.
    let (first, last) = if start <= end {
        (lookup(start), lookup(end))
    } else {
        let last = lookup(end);
        (lookup(start), last)
    };
    (narrow(first.start) as usize, narrow(last.before) as usize)
}

/// `count` consecutive graphemes of one shape: grapheme `i` of the run starts
/// at byte `offset + i·bytes` and column `start + i·cols`, and for `i > 0`
/// the cursor before it is that column too (it opens a cell of its own right
/// after the one before).
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
#[inline]
fn narrow(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl ClusterRun {
    /// Grapheme `i`'s byte offset, start column and cursor-before column.
    #[inline]
    const fn grapheme(self, i: u32) -> ClusterColumns {
        let offset =
            (self.offset as usize).saturating_add((i as usize).saturating_mul(self.bytes as usize));
        let start =
            (self.start as usize).saturating_add((i as usize).saturating_mul(self.cols as usize));
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

    fn finish(self, text_len: usize, total: usize, source: MapSource, ascii: bool) -> ColumnMap {
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
            ascii,
        }
    }
}

impl ColumnMap {
    /// Build a column map from `text` under the `ambiguous` width policy.
    pub fn new(text: &str, ambiguous: AmbiguousWidth) -> Self {
        // Controls (tab, newline, CR) can be zero-width, so they take the full map.
        if printable_ascii(text.as_bytes()) {
            return Self::identity(text.len(), MapSource::Walk(ambiguous), true);
        }

        let mut runs = RunBuilder::new();
        let mut before: usize = 0;
        for g in grid_graphemes_with(text, ambiguous) {
            runs.push(ClusterColumns {
                offset: g.byte_offset,
                start: g.start_col,
                before,
            });
            before = g.end_col;
        }
        runs.finish(
            text.len(),
            before,
            MapSource::Walk(ambiguous),
            text.is_ascii(),
        )
    }

    /// Build a column map from `text` and the grid `cells` it was read from.
    pub fn with_cells(text: &str, cells: &LineCells) -> Self {
        Self::build_cells(text, cells, RunBuilder::new())
    }

    /// Re-measure grid coordinates while reusing this map's run allocation.
    pub(crate) fn refresh_cells(&mut self, text: &str, cells: &LineCells) {
        let mut runs = std::mem::take(&mut self.runs).into_vec();
        runs.clear();
        *self = Self::build_cells(text, cells, RunBuilder { runs });
    }

    fn build_cells(text: &str, cells: &LineCells, mut runs: RunBuilder) -> Self {
        // A row of printable ASCII read one cell per character is the identity.
        if cells.is_identity(text.len()) {
            return Self::identity(text.len(), MapSource::Cells, text.is_ascii());
        }
        let mut spans = cells.iter().take_while(|cell| cell.byte < text.len());
        let mut next = spans.next();
        let mut previous = 0;
        let mut current: Option<CellSpan> = None;
        for (offset, grapheme) in text.grapheme_indices() {
            while next.is_some_and(|cell| cell.byte <= offset) {
                previous = current.map_or(0, |cell| cell.end_col);
                current = next;
                next = spans.next();
            }
            let (start, before) = stored_coordinates(offset, grapheme, current, previous);
            runs.push(ClusterColumns {
                offset,
                start,
                before,
            });
        }
        let last = spans.last().or(next).or(current);
        let total = last.map_or(0, |cell| cell.end_col);
        runs.finish(text.len(), total, MapSource::Cells, text.is_ascii())
    }

    /// The identity map over `len` bytes: every byte one grapheme one column
    /// wide, which needs no runs.
    #[inline]
    fn identity(len: usize, source: MapSource, ascii: bool) -> Self {
        Self {
            runs: Box::default(),
            text_len: narrow(len),
            total: narrow(len),
            source,
            ascii,
        }
    }

    /// Source from which this map's columns were measured.
    #[inline]
    #[must_use]
    pub(crate) const fn source(&self) -> MapSource {
        self.source
    }

    /// Whether the text these columns describe is ASCII.
    #[inline]
    pub(crate) const fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// Exclusive byte limit for qualifying start columns in an identity map of `len` bytes.
    /// Saturated or non-identity maps leave column filtering to the visitor.
    #[inline]
    pub(crate) fn identity_start_limit(
        &self,
        len: usize,
        col: usize,
        inclusive: bool,
    ) -> Option<usize> {
        (self.runs.is_empty() && self.text_len as usize == len)
            .then(|| col.saturating_add(usize::from(inclusive)).min(len))
    }

    /// Byte window containing exactly the eligible needle starts of an identity map.
    #[inline]
    pub(crate) fn identity_match_window<const REV: bool>(
        &self,
        len: usize,
        col: usize,
        inclusive: bool,
        needle_len: usize,
    ) -> Option<std::ops::Range<usize>> {
        let limit =
            self.identity_start_limit(len, col, if REV { inclusive } else { !inclusive })?;
        Some(if REV {
            0..if limit == 0 {
                0
            } else {
                (limit - 1).saturating_add(needle_len).min(len)
            }
        } else {
            limit..len
        })
    }

    /// Display column span `(start_col, end_col)` (`end_col` exclusive) for byte range `start..end`.
    #[must_use]
    #[inline]
    pub fn span(&self, start: usize, end: usize) -> (usize, usize) {
        if self.runs.is_empty() {
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
            return ClusterColumns {
                offset: byte_offset,
                start: byte_offset,
                before: byte_offset,
            };
        }
        let idx = self
            .runs
            .partition_point(|run| run.offset as usize <= byte_offset)
            .saturating_sub(1);
        let Some(&run) = self.runs.get(idx) else {
            return end;
        };
        let into = byte_offset.saturating_sub(run.offset as usize);
        let (i, inside) = match std::num::NonZeroUsize::new(usize::from(run.bytes)) {
            Some(bytes) if run.count > 1 => (into / bytes, into % bytes != 0),
            _ => (0, into != 0),
        };
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if !inside && i < run.count {
            return run.grapheme(i);
        }
        let next = i.saturating_add(1);
        if next < run.count {
            return run.grapheme(next);
        }
        self.runs
            .get(idx.saturating_add(1))
            .map_or(end, |run| run.grapheme(0))
    }

    /// Heap bytes held by this map's run slice.
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.runs
            .len()
            .saturating_mul(std::mem::size_of::<ClusterRun>())
    }
}

/// Translate monotone lowered-byte offsets, rounding up to an original character boundary.
/// Each source character is visited at most once, with no heap storage.
#[derive(Clone)]
struct LowerByteCursor<'a, const REV: bool> {
    remaining: std::str::Chars<'a>,
    lower: usize,
    original: usize,
    /// A reverse visit may stop inside this character's lowered expansion.
    pending: Option<(usize, usize)>,
}

/// Independent endpoint cursors, sharing progress across gaps between matches.
pub(crate) struct LowerByteCursors<'a, const REV: bool> {
    start: LowerByteCursor<'a, REV>,
    end: LowerByteCursor<'a, REV>,
}

impl<'a, const REV: bool> LowerByteCursors<'a, REV> {
    /// `lower_len` is the byte length of the already folded source.
    pub fn new(original: &'a str, lower_len: usize) -> Self {
        let start = LowerByteCursor::new(original, lower_len);
        Self {
            end: start.clone(),
            start,
        }
    }

    /// Starts and ends must each follow the selected order. They can interleave
    /// across overlapping matches, so only copy progress toward the next endpoint.
    pub fn span(&mut self, start: usize, end: usize) -> (usize, usize) {
        if REV {
            let end = self.end.map_to_original(end);
            if self.end.lower < self.start.lower {
                self.start = self.end.clone();
            }
            (self.start.map_to_original(start), end)
        } else {
            let start = self.start.map_to_original(start);
            if self.start.lower > self.end.lower {
                self.end = self.start.clone();
            }
            (start, self.end.map_to_original(end))
        }
    }
}

impl<'a, const REV: bool> LowerByteCursor<'a, REV> {
    /// Offsets passed to [`Self::map_to_original`] follow the selected direction;
    /// repeated offsets are allowed.
    fn new(original: &'a str, lower_len: usize) -> Self {
        Self {
            remaining: original.chars(),
            lower: if REV { lower_len } else { 0 },
            original: if REV { original.len() } else { 0 },
            pending: None,
        }
    }

    /// Translate the next offset in the selected order; offsets past the end clamp to it.
    fn map_to_original(&mut self, lower_byte: usize) -> usize {
        if !REV {
            if self.lower >= lower_byte {
                return self.original;
            }
            for ch in self.remaining.by_ref() {
                self.lower = self.lower.saturating_add(lower_byte_len(ch));
                self.original = self.original.saturating_add(ch.len_utf8());
                if self.lower >= lower_byte {
                    break;
                }
            }
            return self.original;
        }
        loop {
            let (original_len, lower_len) = match self.pending {
                Some(ready) => ready,
                None => {
                    let next = self.remaining.next_back();
                    let Some(ch) = next else {
                        return self.original;
                    };
                    let ready = (ch.len_utf8(), lower_byte_len(ch));
                    self.pending = Some(ready);
                    ready
                }
            };
            let previous = self.lower.saturating_sub(lower_len);
            if previous < lower_byte {
                return self.original;
            }
            self.lower = previous;
            self.original = self.original.saturating_sub(original_len);
            self.pending = None;
        }
    }
}

/// ASCII lowercasing always preserves its one-byte width.
#[inline]
fn lower_byte_len(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else {
        lower_fold_char(ch).map(char::len_utf8).sum()
    }
}

#[cfg(test)]
#[path = "grapheme_tests.rs"]
mod tests;
