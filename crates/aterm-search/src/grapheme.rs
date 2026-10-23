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
    // identity holds under either policy.
    if text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
        return text.len();
    }
    aterm_grapheme::str_grid_columns_with(text, ambiguous)
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
    /// The graphemes, in byte order, as runs of a repeating shape.
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
}

const _: () = assert!(std::mem::size_of::<ColumnMap>() <= 32);

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

    fn finish(self, text_len: usize, total: usize, source: MapSource) -> ColumnMap {
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
    /// Build a column map from `text` under the `ambiguous` width policy.
    pub fn new(text: &str, ambiguous: AmbiguousWidth) -> Self {
        // Controls (tab, newline, CR) can be zero-width, so they take the full map.
        if text.bytes().all(|byte| matches!(byte, b' '..=b'~')) {
            return Self::identity(text.len(), MapSource::Walk(ambiguous));
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
        runs.finish(text.len(), before, MapSource::Walk(ambiguous))
    }

    /// Build a column map from `text` and the grid `cells` it was read from.
    pub fn with_cells(text: &str, cells: &LineCells) -> Self {
        // A row of printable ASCII read one cell per character is the identity.
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
                Some(cell) => (cell.end_col, cell.end_col),
            };
            runs.push(ClusterColumns {
                offset,
                start,
                before,
            });
        }
        let last = spans.last().or(current);
        let total = last.map_or(0, |cell| cell.end_col);
        runs.finish(text.len(), total, MapSource::Cells)
    }

    /// The identity map over `len` bytes: every byte one grapheme one column
    /// wide, which needs no runs.
    #[inline]
    fn identity(len: usize, source: MapSource) -> Self {
        Self {
            runs: Box::default(),
            text_len: narrow(len),
            total: narrow(len),
            source,
        }
    }

    /// Source from which this map's columns were measured.
    #[inline]
    #[must_use]
    pub(crate) const fn source(&self) -> MapSource {
        self.source
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

/// Lowered-byte-to-original-byte offset map for case-insensitive search.
pub(crate) struct LowerByteMap {
    /// (lower_byte_offset, original_byte_offset) pairs at char boundaries,
    /// sorted by lower_byte_offset.
    entries: Vec<(usize, usize)>,
}

impl LowerByteMap {
    /// Build a lower-byte map from `original` in O(C).
    pub fn new(original: &str) -> Self {
        let mut entries = Vec::new();
        let mut orig_offset: usize = 0;
        let mut low_offset: usize = 0;
        for ch in original.chars() {
            entries.push((low_offset, orig_offset));
            let orig_len = ch.len_utf8();
            let low_len: usize = lower_fold_char(ch).map(|c| c.len_utf8()).sum();
            orig_offset = orig_offset.saturating_add(orig_len);
            low_offset = low_offset.saturating_add(low_len);
        }
        entries.push((low_offset, orig_offset));
        Self { entries }
    }

    /// Map `lower_byte` to the corresponding byte offset in the original text in O(log C).
    #[inline]
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

    /// The per-grapheme map the runs encode (one `(offset, start, before)` entry per
    /// grapheme plus a sentinel, binary-searched): the oracle the encoding must match.
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

    /// The run-length encoded map answers every `(start, end)` byte pair as the
    /// per-grapheme reference does, under both `ambiguous_width` policies: on every
    /// pair and triple of shapes, and on long lines where a shape repeats then changes.
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
    /// cells only change an answer where the grid did something else.
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

        let orphans = "\u{301}ab\u{301}";
        let mut cells = LineCells::new();
        cells.push(2..3, 0, 1);
        cells.push(3..4, 1, 1);
        let map = ColumnMap::with_cells(orphans, &cells);
        assert_eq!(map.span(0, 2), (0, 0));
        assert_eq!(map.span(2, 4), (0, 2));
        assert_eq!(map.span(4, 6), (2, 2));
    }

    /// A line of one shape is one run however long it is, and runs are sized exactly:
    /// a 100-ideograph row, 100 decomposed accents, or an ASCII row with one
    /// box-drawing glyph holds a few dozen bytes.
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
        assert_eq!(display_columns("\0\t\r\n\u{7f}"), 0);
        assert_eq!(display_columns("日"), 2);
        assert_eq!(display_columns("न्दी"), 3);
        assert_eq!(display_columns("1\u{fe0f}\u{20e3}"), 2);
        assert_eq!(display_columns("👩🏽‍💻"), 2);
    }

    /// `ColumnMap::end_column` answers every byte offset as the O(G) scan
    /// `byte_to_column` does: ASCII, wide CJK, combining marks, emoji,
    /// controls, and a long CJK line (the O(log G) lookup path).
    #[test]
    fn column_map_matches_byte_to_column() {
        let long: String = "日本語テスト漢字表示".chars().cycle().take(80).collect();
        let texts = [
            "",
            "hello world, this is a test line!",
            "ab日本語cd",
            "abc\u{0301}def",
            "\u{1F600}hello\u{1F600}",
            "日a本b語c",
            "a\u{0308}\u{0301}b",
            "\t\n\r abc",
            long.as_str(),
        ];
        for text in texts {
            let map = ColumnMap::new(text, AmbiguousWidth::Single);
            for offset in 0..=text.len() {
                assert_eq!(
                    map.end_column(offset),
                    byte_to_column(text, offset),
                    "{text:?} at offset {offset}"
                );
            }
        }
    }

    /// `LowerByteMap` maps every lowered byte offset back as the O(C) scan
    /// `map_lower_byte_to_original` does, including the Kelvin sign (3 bytes
    /// lowered to 1), Greek, CJK, the empty line and a long mixed line.
    #[test]
    fn lower_byte_map_matches_map_lower_byte_to_original() {
        let long: String = "Hello日本語World".chars().cycle().take(100).collect();
        let texts = [
            "Hello World Test",
            "a\u{212A}b",
            "ABCdef",
            "Straße",
            "ΔΕΛΤΑ",
            "日本語ABC",
            "",
            long.as_str(),
        ];
        for text in texts {
            let map = LowerByteMap::new(text);
            for offset in 0..=text.to_lowercase().len() {
                assert_eq!(
                    map.map_to_original(offset),
                    map_lower_byte_to_original(text, offset),
                    "{text:?} at lower offset {offset}"
                );
            }
        }
    }
}
