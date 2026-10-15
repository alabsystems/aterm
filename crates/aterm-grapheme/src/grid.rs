// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The terminal grid's layout of a line, one character at a time.
//!
//! The grid (`aterm_core`'s `handler_write`) advances its cursor per
//! CHARACTER, and whether a character opens a cell or joins the one before it
//! depends on what that cell already holds: its base, whether it is drawn
//! wide, whether its last stored character is a ZWJ. That state crosses UAX #29
//! cluster boundaries (Sinhala `ශ්‍රී` ends one cluster with a ZWJ and the grid
//! folds the next cluster's first letter into the same cell), so a measurer
//! that charges each cluster on its own disagrees with the grid. Every column
//! measurer walks a line with one [`GridColumns`] instead.

use aterm_types::text_shaping::AmbiguousWidth;

use crate::emoji_presentation::is_vs16_emoji_capable;
use crate::grapheme_iter::{GraphemeClusters, GraphemeIndices};
use crate::tables::width as unicode_tables;
use crate::width::{is_emoji_modifier_base, is_regional_indicator, is_skin_tone_modifier};

const VS15: char = '\u{FE0E}';
const VS16: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';

/// Characters one cell stores after its base (`aterm_grid`'s
/// `CellExtra::MAX_COMBINING`). The grid drops the rest, so a dropped
/// character changes neither the cell's last character nor its flag count.
const MAX_JOINED: u8 = 16;

/// The cell the last printing character opened, as the grid holds it.
#[derive(Clone, Copy, Debug)]
struct Cell {
    /// Its first column.
    col: usize,
    base: char,
    /// Drawn wide: VS16 sets it, VS15 clears it, and only a wide cell takes a
    /// skin-tone modifier.
    wide: bool,
    /// Regional indicators stored in it, the base included: a flag is two.
    ris: u8,
    /// Characters stored after the base.
    joined: u8,
    /// The last stored character is a ZWJ (`should_combine_with_previous_zwj`).
    ends_zwj: bool,
}

impl Cell {
    fn open(col: usize, base: char, wide: bool) -> Self {
        Self {
            col,
            base,
            wide,
            ris: u8::from(is_regional_indicator(base)),
            joined: 0,
            ends_zwj: false,
        }
    }

    /// Store `c` after the base, as `CellExtra::add_combining` does.
    fn store(&mut self, c: char) {
        if self.joined < MAX_JOINED {
            self.joined += 1;
            self.ends_zwj = c == ZWJ;
            self.ris = self.ris.saturating_add(u8::from(is_regional_indicator(c)));
        }
    }

    /// Whether the grid folds printing character `c` into this cell rather
    /// than open a new one, in `write_char`'s order: a skin-tone modifier onto
    /// a wide `Emoji_Modifier_Base`, a flag's second regional indicator onto
    /// its first, and any non-ASCII character after a stored ZWJ. The grid
    /// folds printable ASCII after a ZWJ on no path (UAX #29 GB11 joins a ZWJ
    /// only to Extended_Pictographic, and no ASCII character is one): neither
    /// the run writer (`print_ascii_bulk`) nor `write_char` under IRM, DECAWM
    /// off or a national charset.
    fn folds(&self, c: char) -> bool {
        (is_skin_tone_modifier(c) && self.wide && is_emoji_modifier_base(self.base))
            || (is_regional_indicator(c) && is_regional_indicator(self.base) && self.ris < 2)
            || (self.ends_zwj && !c.is_ascii())
    }
}

/// What one character did to the grid (see [`GridColumns::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridStep {
    /// A printing character opened a cell of its own at column `col`, `width`
    /// columns wide.
    Opens {
        /// The cell's first column.
        col: usize,
        /// Its width, 1 or 2.
        width: usize,
    },
    /// A printing character the grid folded into the cell before it, at
    /// column `col`: after a stored ZWJ, a skin tone onto its base, a flag's
    /// second half. It is drawn in that cell and moves no cursor.
    Folds {
        /// The first column of the cell it joined.
        col: usize,
    },
    /// A zero-width character stored in the cell before it (with none, it
    /// lands under the cursor and keeps nothing).
    Joins {
        /// VS16 widened that cell from 1 column to 2, moving the cursor one.
        widened: bool,
    },
}

impl GridStep {
    /// Columns the cursor advanced.
    #[must_use]
    pub const fn advance(self) -> usize {
        match self {
            Self::Opens { width, .. } => width,
            Self::Folds { .. } | Self::Joins { widened: false } => 0,
            Self::Joins { widened: true } => 1,
        }
    }
}

/// The grid's layout of one line, fed a character at a time from the line's
/// first column.
///
/// Each printing character takes [`char_width`](crate::char_width) cells
/// ([`char_width_cjk`](crate::char_width_cjk) under
/// [`AmbiguousWidth::Double`], the terminal's `ambiguous_width = "wide"`), except
/// that
///
/// * a zero-width character joins the cell before it, and VS16 widens that cell
///   from 1 to 2 when its base [`is_vs16_emoji_capable`]
///   (`widen_previous_cell_for_vs16`), `©️ ▶️ ℹ️ 1️⃣` as much as `❤️`;
/// * VS15 narrows a wide cell's glyph without moving the cursor; the emptied
///   column after it becomes the cell later characters join
///   (`narrow_previous_cell_for_vs15`);
/// * a skin-tone modifier folds into a wide [`is_emoji_modifier_base`] cell
///   (`try_combine_skin_tone_modifier`); after anything else it is its own
///   2-cell glyph;
/// * a regional indicator folds into a cell holding a lone one (a flag);
/// * a non-ASCII character folds into a cell whose last stored character is a
///   ZWJ (`should_combine_with_previous_zwj`), whatever the cell holds and
///   wherever UAX #29 puts the cluster boundary: `❤‍🔥` keeps its narrow
///   base's one cell, `👨‍👩‍👧` its first person's two, and Sinhala `ක්‍ය` is one
///   cell though it is two clusters.
///
/// `aterm-core/tests/grid_column_agreement.rs` checks this against the real
/// grid. A line edge (VS16 on the last column, a fold across a soft wrap) is
/// beyond a measurer that is not told the grid's width.
#[derive(Clone, Debug, Default)]
pub struct GridColumns {
    /// The cursor column.
    cursor: usize,
    /// The cell a joining character lands in; `None` before the first one.
    cell: Option<Cell>,
    /// How wide the grid draws an East Asian Ambiguous character: the
    /// terminal's `ambiguous_width` policy the line was written under.
    ambiguous: AmbiguousWidth,
}

impl GridColumns {
    /// A walk from a line's first column, East Asian Ambiguous characters one
    /// cell wide (the terminal's default).
    #[must_use]
    pub const fn new() -> Self {
        Self::with_ambiguous_width(AmbiguousWidth::Single)
    }

    /// A walk from a line's first column, East Asian Ambiguous characters as
    /// wide as `ambiguous` says: two cells under [`AmbiguousWidth::Double`],
    /// as the grid writes them with `ambiguous_width = "wide"`.
    #[must_use]
    pub const fn with_ambiguous_width(ambiguous: AmbiguousWidth) -> Self {
        Self {
            cursor: 0,
            cell: None,
            ambiguous,
        }
    }

    /// The cursor column: the cells everything fed so far occupies.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.cursor
    }

    /// Lay out `c` after everything fed so far.
    pub fn step(&mut self, c: char) -> GridStep {
        let width = match self.ambiguous {
            AmbiguousWidth::Double => unicode_tables::char_width_cjk(c),
            _ => unicode_tables::char_width(c),
        };
        match self.cell.as_mut() {
            Some(cell) if width == 0 => {
                cell.store(c);
                if c == VS16 && !cell.wide && is_vs16_emoji_capable(cell.base) {
                    cell.wide = true;
                    self.cursor = self.cursor.saturating_add(1);
                    return GridStep::Joins { widened: true };
                }
                if c == VS15 && cell.wide {
                    // The glyph narrows; the cursor stays past the emptied
                    // column, which is now the cell before it.
                    *cell = Cell::open(cell.col.saturating_add(1), ' ', false);
                }
                GridStep::Joins { widened: false }
            }
            Some(cell) if cell.folds(c) => {
                cell.store(c);
                GridStep::Folds { col: cell.col }
            }
            // Nothing before it: a zero-width character lands on the cell
            // under the cursor, which the next printing character overwrites.
            None if width == 0 => GridStep::Joins { widened: false },
            _ => {
                let col = self.cursor;
                self.cell = Some(Cell::open(col, c, width >= 2));
                self.cursor = self.cursor.saturating_add(width);
                GridStep::Opens { col, width }
            }
        }
    }

    /// Lay out every character of `s`; returns the columns the cursor advanced.
    pub fn advance_str(&mut self, s: &str) -> usize {
        let before = self.cursor;
        for c in s.chars() {
            self.step(c);
        }
        self.cursor.saturating_sub(before)
    }
}

/// The number of grid cells a terminal lays `s` out in, written from the first
/// column of a line.
///
/// Can exceed `s`'s display width: a Devanagari conjunct is one UAX #29
/// cluster the grid paints in three cells (`न्`, `द`, `ी`).
#[must_use]
pub fn str_grid_columns(s: &str) -> usize {
    str_grid_columns_with(s, AmbiguousWidth::Single)
}

/// [`str_grid_columns`] under the terminal's `ambiguous_width` policy.
#[must_use]
pub fn str_grid_columns_with(s: &str, ambiguous: AmbiguousWidth) -> usize {
    GridColumns::with_ambiguous_width(ambiguous).advance_str(s)
}

/// One grapheme cluster and the grid columns it is drawn in (see
/// [`grid_graphemes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridGrapheme<'a> {
    /// The cluster.
    pub text: &'a str,
    /// Its byte offset in the line.
    pub byte_offset: usize,
    /// The first column it is drawn in: the cursor column before it, or,
    /// when the grid folds its first character into the cell before it, that
    /// cell's first column.
    pub start_col: usize,
    /// The cursor column after it. `start_col..end_col` is every column the
    /// cluster is drawn in: the cell it folded into, and the cells it opened.
    /// A cluster of zero-width characters only (a ZWSP, a mark after a
    /// control) is drawn in none.
    pub end_col: usize,
}

/// Iterator over a line's grapheme clusters with their grid columns.
#[derive(Clone, Debug)]
pub struct GridGraphemes<'a> {
    clusters: GraphemeIndices<'a>,
    grid: GridColumns,
}

impl<'a> Iterator for GridGraphemes<'a> {
    type Item = GridGrapheme<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (byte_offset, text) = self.clusters.next()?;
        let mut start_col = self.grid.column();
        let mut chars = text.chars();
        if let Some(GridStep::Folds { col }) = chars.next().map(|c| self.grid.step(c)) {
            start_col = col;
        }
        for c in chars {
            self.grid.step(c);
        }
        Some(GridGrapheme {
            text,
            byte_offset,
            start_col,
            end_col: self.grid.column(),
        })
    }
}

/// `s`'s grapheme clusters, each with the grid columns it is drawn in when `s`
/// is written from the first column of a line.
///
/// The one walk every byte↔column conversion uses, so the find bar, a copied
/// selection and a double-click name the cells the grid painted. Columns are
/// measured in context: a cluster the grid folds into the cell before it (the
/// `ය` of Sinhala `ක්‍ය`, the second `👍` of `👍‍‍👍`) adds no column and is
/// drawn in the cell it joined.
#[must_use]
pub fn grid_graphemes(s: &str) -> GridGraphemes<'_> {
    grid_graphemes_with(s, AmbiguousWidth::Single)
}

/// [`grid_graphemes`] under the terminal's `ambiguous_width` policy: with
/// [`AmbiguousWidth::Double`] an East Asian Ambiguous character (`→`, `─`,
/// `°`) is drawn in two columns, as the grid writes it.
#[must_use]
pub fn grid_graphemes_with(s: &str, ambiguous: AmbiguousWidth) -> GridGraphemes<'_> {
    GridGraphemes {
        clusters: s.grapheme_indices(),
        grid: GridColumns::with_ambiguous_width(ambiguous),
    }
}
