// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Display-offset-aware per-row read primitive shared by the render snapshot
//! and the text-extraction path.
//!
//! When the viewport is scrolled back (`display_offset > 0`), [`Grid::row`]
//! returns the historical CELLS (its `row_index` applies the offset) but the
//! coordinate-keyed extras accessors (`cell_render_data`, `complex_char_str_at`,
//! `CellExtras::get`) are display-offset-BLIND — they read the LIVE extras map at
//! the raw visible row. Scrolled-off extras were moved into `ring_extras` /
//! lazy buffer / tiered scrollback at scroll time, so the live map no longer holds
//! them: reading there strands every scrolled emoji / CJK-SMP as U+FFFD and drops
//! combining marks, truecolor RGB, and hyperlinks. It also MISALIGNS the still-live
//! bottom rows during a PARTIAL scroll — their cell comes from `visible_row -
//! display_offset` but their extras were read at the raw `visible_row`.
//!
//! This primitive resolves live-vs-history ONCE per row and routes history rows
//! through the unified 3-tier materializer ([`Grid::materialize_scrollback_row_full`]),
//! so both the render and text paths read correctly-paired extras across every tier.
//! At `display_offset == 0` it is byte-identical to the pre-existing live read
//! (`screen_row == visible_row`, `Live` arm forwards to the same accessors).

use std::borrow::Cow;

use super::{Grid, MaterializedRow};
use crate::extra_collection::CellRenderData;
use crate::{Cell, CellCoord, CellExtra, Row};

/// A single viewport row resolved against the current `display_offset`.
pub enum VisibleRowView<'a> {
    /// A still-live row. `row` already carries display-offset-mapped CELLS (from
    /// [`Grid::row`]); `screen_row` is the LIVE-extras key (`visible_row -
    /// display_offset`), which equals `visible_row` when not scrolled.
    Live {
        grid: &'a Grid,
        screen_row: u16,
        row: &'a Row,
    },
    /// A scrolled-off history row, materialized (cells + paired extras) through
    /// the unified 3-tier reader.
    History { mat: MaterializedRow },
    /// No such row (out of range, or the history index was unavailable).
    Empty,
}

/// Per-cell extras source: the live coordinated probe, or a materialized history
/// cell's extras. Both expose the SAME channels so the render / text leaves read
/// one shape regardless of tier. A materialized [`CellExtra`] is deliberately
/// stored in the same layout as a live one (complex string + separate combining
/// marks + RGB), so the History arm needs no reshaping.
#[derive(Clone, Copy)]
pub enum CellDataView<'a> {
    Live(CellRenderData<'a>),
    History(Option<&'a CellExtra>),
}

impl<'a> CellDataView<'a> {
    /// Foreground RGB overflow color, if any.
    #[must_use]
    pub fn fg_rgb(self) -> Option<[u8; 3]> {
        match self {
            Self::Live(r) => r.fg_rgb(),
            Self::History(e) => e.and_then(|x| x.fg_rgb()),
        }
    }

    /// Background RGB overflow color, if any.
    #[must_use]
    pub fn bg_rgb(self) -> Option<[u8; 3]> {
        match self {
            Self::Live(r) => r.bg_rgb(),
            Self::History(e) => e.and_then(|x| x.bg_rgb()),
        }
    }

    /// Base codepoint for a complex cell (the first scalar of the complex
    /// string). Mirrors [`CellRenderData::complex_char`] for both tiers.
    #[must_use]
    pub fn complex_base(self) -> Option<char> {
        match self {
            Self::Live(r) => r.complex_char(),
            Self::History(e) => e
                .and_then(|x| x.complex_char())
                .and_then(|s| s.chars().next()),
        }
    }

    /// The cell's [`CellExtra`] (underline color, hyperlink). For combining
    /// marks / cluster tails use [`marks`](Self::marks) — the live and history
    /// representations differ (see there).
    #[must_use]
    pub fn cell_extra(self) -> Option<&'a CellExtra> {
        match self {
            Self::Live(r) => r.cell_extra(),
            Self::History(e) => e,
        }
    }

    /// The overlay marks for the render fold: combining diacritics, or the
    /// trailing scalars of an emoji cluster (ZWJ / skin-tone / flag), matching
    /// what the live cluster/combining classifier reads.
    ///
    /// LIVE keeps `(base, combining marks)` split, so this is `combining()`.
    /// A MATERIALIZED cell folds the whole grapheme into its complex string
    /// (base = first scalar), so the marks are `complex_char.chars().skip(1)`
    /// (plus any separately-stored combining, normally none) — reconstructing
    /// the same `(base, marks)` pair the live path would produce.
    #[must_use]
    pub fn marks(self) -> Cow<'a, [char]> {
        match self {
            Self::Live(r) => r
                .cell_extra()
                .map_or(Cow::Borrowed(&[][..]), |e| Cow::Borrowed(e.combining())),
            Self::History(e) => match e.and_then(CellExtra::complex_char) {
                Some(s) => {
                    let mut v: Vec<char> = s.chars().skip(1).collect();
                    if let Some(x) = e {
                        v.extend_from_slice(x.combining());
                    }
                    Cow::Owned(v)
                }
                None => e.map_or(Cow::Borrowed(&[][..]), |x| Cow::Borrowed(x.combining())),
            },
        }
    }
}

impl<'a> VisibleRowView<'a> {
    /// Effective column count of this row (matches `Row::len` / `MaterializedRow::len`).
    #[must_use]
    pub fn len(&self) -> u16 {
        match self {
            Self::Live { row, .. } => row.len(),
            Self::History { mat } => mat.len(),
            Self::Empty => 0,
        }
    }

    /// Whether the row has no occupied columns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this row is a soft-wrap continuation of the previous line
    /// (matches `Row::is_wrapped` for live rows and the source `Line`'s wrap
    /// flag for materialized history rows). `false` for an empty/out-of-range
    /// row — callers distinguish that via the `Empty` variant.
    #[must_use]
    pub fn is_wrapped(&self) -> bool {
        match self {
            Self::Live { row, .. } => row.is_wrapped(),
            Self::History { mat } => mat.is_wrapped(),
            Self::Empty => false,
        }
    }

    /// The cell at `col`, or `None` if out of range.
    #[must_use]
    pub fn cell(&self, col: u16) -> Option<Cell> {
        match self {
            Self::Live { row, .. } => row.get(col).copied(),
            Self::History { mat } => mat.cells.get(col as usize).copied(),
            Self::Empty => None,
        }
    }

    /// Whether the cell at `col` is the blank right half of a wide glyph.
    ///
    /// Same rule as `Row::is_cell_wide_continuation` (DECSCA-protected cells
    /// share bit 10 with WIDE_CONTINUATION, so a real continuation must not be
    /// itself WIDE and must sit immediately right of a WIDE main cell), applied
    /// uniformly to live and materialized rows.
    #[must_use]
    pub fn is_wide_continuation(&self, col: u16) -> bool {
        self.cell(col)
            .is_some_and(|c| c.is_wide_continuation() && !c.is_wide())
            && col > 0
            && self.cell(col - 1).is_some_and(|c| c.is_wide())
    }

    /// The coordinated extras view for `col`. `cell` is passed so the live probe
    /// can flag-gate (and the history arm can assert its invariants).
    #[must_use]
    pub fn cell_data(&self, col: u16, cell: Cell) -> CellDataView<'_> {
        match self {
            Self::Live {
                grid, screen_row, ..
            } => CellDataView::Live(grid.cell_render_data(*screen_row, col, cell)),
            Self::History { mat } => {
                // Materialize bakes StyleId colors inline, so a history cell must
                // never take the render StyleId branch (which resolves against the
                // LIVE StyleTable). Guard the premise in debug builds.
                debug_assert!(
                    !cell.uses_style_id(),
                    "materialized history cell must have colors baked inline, not a StyleId"
                );
                CellDataView::History(mat.get_extra(col))
            }
            Self::Empty => CellDataView::History(None),
        }
    }

    /// Append this cell's resolved text (complex string or plain char, then any
    /// combining marks) to `out`, reading extras from the correct tier. The Live
    /// arm mirrors the historical `row_text_into` inner block verbatim but keyed
    /// on `screen_row`; at `display_offset == 0` (`screen_row == visible_row`) it
    /// is byte-identical.
    pub fn push_cell_text(&self, col: u16, cell: Cell, out: &mut String) {
        match self {
            Self::Live {
                grid, screen_row, ..
            } => {
                if cell.is_complex() {
                    match grid.complex_char_str_at(*screen_row, col) {
                        Some(cs) => out.push_str(&cs),
                        None => out.push('\u{FFFD}'),
                    }
                } else {
                    let ch = cell.char();
                    out.push(if ch == '\0' { ' ' } else { ch });
                }
                // Combining marks in CellExtra (HashMap). Guarded with has_extras()
                // + non-empty map exactly as the historical read, to skip stale
                // entries from overwritten cells (#7456).
                if cell.has_extras()
                    && !grid.storage.extras.is_empty()
                    && let Some(extra) = grid.storage.extras.get(CellCoord::new(*screen_row, col))
                {
                    for &mark in extra.combining() {
                        out.push(mark);
                    }
                }
            }
            Self::History { mat } => {
                let ex = mat.get_extra(col);
                if cell.is_complex() {
                    match ex.and_then(CellExtra::complex_char) {
                        Some(s) => out.push_str(s),
                        None => out.push('\u{FFFD}'),
                    }
                } else {
                    let ch = cell.char();
                    out.push(if ch == '\0' { ' ' } else { ch });
                }
                if let Some(ex) = ex {
                    for &mark in ex.combining() {
                        out.push(mark);
                    }
                }
            }
            Self::Empty => {}
        }
    }
}

impl Grid {
    /// Resolve a viewport row honoring `display_offset`: a [`Live`] row (cells +
    /// display-mapped extras key) when the viewport is not scrolled past it, else
    /// a [`History`] row materialized through the unified 3-tier reader.
    ///
    /// `display_offset == 0` always yields `Live` with `screen_row == visible_row`
    /// — byte-identical to reading `Grid::row` + the live extras accessors directly.
    ///
    /// [`Live`]: VisibleRowView::Live
    /// [`History`]: VisibleRowView::History
    #[must_use]
    pub fn visible_row_view(&self, visible_row: u16) -> VisibleRowView<'_> {
        let d = self.display_offset();
        if usize::from(visible_row) >= d {
            // Live row: `Grid::row` display-maps the CELLS; the extras live-map key
            // is `visible_row - display_offset`. Live implies d <= visible_row, so
            // d fits u16 and the subtraction cannot underflow.
            let screen_row = visible_row.saturating_sub(u16::try_from(d).unwrap_or(0));
            match self.row(visible_row) {
                Some(row) => VisibleRowView::Live {
                    grid: self,
                    screen_row,
                    row,
                },
                None => VisibleRowView::Empty,
            }
        } else {
            // History row: rev_idx 0 == newest scrolled-off line. Single tier-count-
            // independent handle — `try_get_history_line` splits the tiers internally.
            let rev_idx = d - 1 - usize::from(visible_row);
            match self.materialize_scrollback_row_full(rev_idx, self.cols()) {
                Some(mat) => VisibleRowView::History { mat },
                None => VisibleRowView::Empty,
            }
        }
    }

    /// The LIVE-frame twin of [`visible_row_view`](Self::visible_row_view): the row
    /// view at `screen_row` IGNORING `display_offset` (as if the viewport were at the
    /// bottom), so its cells + extras are always the live on-screen row. This is the
    /// offset-independent frame the socket introspection reads
    /// (`cell`/`screen`/`cells`) share with [`row_at_screen`](Self::row_at_screen) and
    /// the live text extraction — so a color/attr/wide read cannot pair with a
    /// scrolled-back row while the glyph comes from the live one. At
    /// `display_offset == 0` it is identical to `visible_row_view`.
    #[must_use]
    pub fn screen_row_view(&self, screen_row: u16) -> VisibleRowView<'_> {
        match self.row_at_screen(screen_row) {
            Some(row) => VisibleRowView::Live {
                grid: self,
                screen_row,
                row,
            },
            None => VisibleRowView::Empty,
        }
    }

    /// The live on-screen row at `screen_row`, IGNORING `display_offset` (as if
    /// the viewport were at the bottom) — the row reference behind
    /// [`row_text_screen_into`](Self::row_text_screen_into), for callers that
    /// need cell-level LIVE-frame reads (terminal-relative text extraction,
    /// whose extras lookups are keyed by the live visible row).
    #[must_use]
    pub fn row_at_screen(&self, screen_row: u16) -> Option<&Row> {
        self.storage.row_at_screen(screen_row)
    }

    /// LIVE-frame twin of [`is_wide_continuation_at`](Self::is_wide_continuation_at):
    /// same DECSCA-disambiguated continuation rule, keyed on the SCREEN row
    /// (offset-independent) so it pairs with [`row_at_screen`](Self::row_at_screen).
    #[must_use]
    #[inline]
    pub fn is_wide_continuation_at_screen(&self, screen_row: u16, col: u16) -> bool {
        self.storage
            .row_at_screen(screen_row)
            .is_some_and(|r| r.is_cell_wide_continuation(col))
    }

    /// Offset-INDEPENDENT read of the live on-screen row at `screen_row` (as if
    /// `display_offset == 0`), for absolute-frame callers (block command/output
    /// text extraction) that must NOT follow the scroll position. Unlike
    /// [`row_text_into`](Self::row_text_into) — which is display-offset-aware and
    /// returns the SCROLLED view — this always reads the live grid row + its live
    /// extras, so a user scrolled at read time cannot inject / duplicate history.
    pub fn row_text_screen_into(&self, screen_row: u16, out: &mut String) -> bool {
        out.clear();
        let Some(row) = self.storage.row_at_screen(screen_row) else {
            return false;
        };
        let view = VisibleRowView::Live {
            grid: self,
            screen_row,
            row,
        };
        let len = view.len();
        out.reserve(len as usize);
        for col in 0..len {
            let Some(cell) = view.cell(col) else {
                continue;
            };
            if view.is_wide_continuation(col) {
                continue;
            }
            view.push_cell_text(col, cell, out);
        }
        true
    }

    /// Allocating twin of [`row_text_screen_into`](Self::row_text_screen_into).
    #[must_use]
    pub fn row_text_screen(&self, screen_row: u16) -> Option<String> {
        let mut s = String::new();
        self.row_text_screen_into(screen_row, &mut s).then_some(s)
    }
}
