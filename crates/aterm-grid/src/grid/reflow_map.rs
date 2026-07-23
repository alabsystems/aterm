// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Reflow utility functions and copy-time extras remapping.
//!
//! Pure helper functions (chunk boundary computation, cell copying) plus
//! optional source-coordinate tracking for preserving `CellExtras` during
//! reflow (#3977). Extracted from `reflow.rs` to stay under the 500-line
//! file limit.

use crate::CellExtra;
use crate::CellFlags;
#[cfg(any(test, feature = "testing"))]
use crate::test_counters::count_reflow_cell_ops;

/// How to map a copied cell's source index back to its original [`CellCoord`]
/// during reflow extras-remapping. A STRAIGHT single-row copy is pure index
/// arithmetic (`Row(r)` → `CellCoord::new(r, col)`), so it needs no allocation —
/// only the continuation-MERGE path, which concatenates cells from several source
/// rows into one buffer, needs the explicit per-index `Coords` table.
#[derive(Clone, Copy)]
pub(super) enum ExtrasSource<'a> {
    /// No extras to remap (the source row had none) — `remap_copied_extra` no-ops.
    None,
    /// Single source row `r`: `src_index` maps to `CellCoord::new(r, src_index)`.
    Row(u16),
    /// Explicit index→coord table (the merge path spans multiple source rows).
    Coords(&'a [CellCoord]),
}

pub(super) struct ExtrasCopyCtx<'a> {
    pub(super) source: ExtrasSource<'a>,
    pub(super) old_extras: Option<&'a CellExtras>,
    pub(super) new_extras: &'a mut CellExtras,
}
use super::row_u16;
use super::{CellCoord, CellExtras, PageStore, Row};

/// Adjust chunk boundary to avoid splitting wide characters, ensuring forward progress.
pub(super) fn adjust_chunk_boundary(
    cells: &[crate::Cell],
    cell_offset: usize,
    chunk_end: usize,
) -> usize {
    let mut actual_end = chunk_end;
    if actual_end < cells.len()
        && actual_end > cell_offset
        && cells[actual_end - 1].flags().contains(CellFlags::WIDE)
    {
        actual_end -= 1;
    }
    if actual_end == cell_offset && cell_offset < cells.len() {
        actual_end = cell_offset + 2.min(cells.len() - cell_offset);
    }
    actual_end
}

/// Check if a cursor at `cursor_logical_offset` falls within a reflow chunk.
///
/// Returns `Some((dest_row, column_in_chunk))` if the cursor (clamped to
/// `cells_len`) is in `[cell_offset, actual_end)` (or `<= actual_end` for the
/// last chunk, so a cursor past the final cell lands on the last row).
pub(super) fn cursor_in_chunk(
    cursor_logical_offset: usize,
    cells_len: usize,
    cell_offset: usize,
    actual_end: usize,
    dest_row: usize,
) -> Option<(usize, u16)> {
    let clamped = cursor_logical_offset.min(cells_len);
    let is_last_chunk = actual_end == cells_len;
    let in_range = clamped >= cell_offset
        && if is_last_chunk {
            clamped <= actual_end
        } else {
            clamped < actual_end
        };
    if in_range {
        Some((dest_row, row_u16(clamped - cell_offset)))
    } else {
        None
    }
}

/// Build source coordinates aligned with a row's physical cell slice.
pub(super) fn source_coords_for_row(row: u16, len: usize) -> Vec<CellCoord> {
    (0..len)
        .map(|col| CellCoord::new(row, row_u16(col)))
        .collect()
}

fn remap_copied_extra(
    extras_ctx: &mut ExtrasCopyCtx<'_>,
    src_index: usize,
    dest_row: u16,
    dest_col: u16,
) {
    let Some(old_extras) = extras_ctx.old_extras else {
        return;
    };
    // Resolve the source coord: single-row copies compute it (no per-row Vec); the
    // merge path indexes its concatenated table. Byte-identical to the old
    // `source_coords.get(src_index)` — a single-row table WAS `CellCoord::new(row,
    // src_index)`, and copies never pass an out-of-range index for that row.
    let coord = match extras_ctx.source {
        ExtrasSource::None => return,
        ExtrasSource::Row(r) => CellCoord::new(r, row_u16(src_index)),
        ExtrasSource::Coords(coords) => match coords.get(src_index) {
            Some(&c) => c,
            None => return,
        },
    };

    // HashMap first: preserves multi-codepoint ZWJ sequences, hyperlinks,
    // combining marks, underline colors, and all other structured extras.
    if let Some(extra) = old_extras.get(coord) {
        extras_ctx
            .new_extras
            .set(CellCoord::new(dest_row, dest_col), extra.clone());
        return;
    }

    // Ring-buffer fallback: the production write path stores non-BMP complex
    // chars and RGB colors exclusively in ring buffers (ComplexCharRing,
    // RgbColorRing) for performance. Without this fallback, emoji and RGB
    // colors written via the hot path are silently lost on column reflow.
    // (#7447)
    let complex = old_extras.complex_codepoint_for(coord.row, coord.col);
    let fg = old_extras.fg_rgb_for(coord.row, coord.col);
    let bg = old_extras.bg_rgb_for(coord.row, coord.col);

    if complex.is_some() || fg.is_some() || bg.is_some() {
        let mut extra = CellExtra::default();
        if let Some(c) = complex {
            use std::sync::Arc;
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            extra.set_complex_char(Some(Arc::from(s)));
        }
        if let Some(rgb) = fg {
            extra.set_fg_rgb(Some(rgb));
        }
        if let Some(rgb) = bg {
            extra.set_bg_rgb(Some(rgb));
        }
        extras_ctx
            .new_extras
            .set(CellCoord::new(dest_row, dest_col), extra);
    }
}

/// Copy cells into a row, replacing wide chars that can't fit in narrow terminals.
pub(super) fn copy_cells_to_row(
    new_row: &mut Row,
    cells: &[crate::Cell],
    cell_offset: usize,
    actual_end: usize,
    new_cols: u16,
    dest_row: u16,
    extras_ctx: &mut ExtrasCopyCtx<'_>,
) {
    #[cfg(any(test, feature = "testing"))]
    count_reflow_cell_ops(actual_end.saturating_sub(cell_offset));

    let mut j = 0u16;
    let mut skip_next_spacer = false;
    for (src_index, cell) in cells[cell_offset..actual_end].iter().enumerate() {
        if cell.flags().contains(CellFlags::WIDE) && new_cols < 2 {
            new_row.set(j, crate::Cell::EMPTY);
            // Cell::EMPTY is_empty() == true, so Row::set() doesn't advance
            // row.len. Force len forward so the replacement space is visible
            // to row_text(), Display, and merge_continuation_rows (#7567).
            new_row.update_len(j.saturating_add(1));
            j += 1;
            skip_next_spacer = true;
        } else if skip_next_spacer && cell.flags().contains(CellFlags::WIDE_CONTINUATION) {
            skip_next_spacer = false;
        } else if j < new_cols {
            let dest_col = j;
            new_row.set(dest_col, *cell);
            remap_copied_extra(extras_ctx, cell_offset + src_index, dest_row, dest_col);
            j += 1;
            skip_next_spacer = false;
        } else {
            skip_next_spacer = false;
        }
    }
}

/// Chunk non-empty cells into new-width rows with cursor tracking.
///
/// The first emitted row is NOT marked wrapped; subsequent rows (from chunking
/// wider content) are marked wrapped. Caller sets the first row's wrapped flag
/// if needed by checking `new_rows[first_idx]` after this returns.
pub(super) fn chunk_cells_to_rows(
    cells: &[crate::Cell],
    new_cols: u16,
    new_pages: &mut PageStore,
    new_rows: &mut Vec<Row>,
    cursor_offset: Option<usize>,
    cursor_state: &mut (usize, u16),
    extras_ctx: &mut ExtrasCopyCtx<'_>,
) {
    let mut cell_offset = 0;
    let mut first = true;
    while cell_offset < cells.len() {
        let chunk_end = (cell_offset + usize::from(new_cols)).min(cells.len());
        let actual_end = adjust_chunk_boundary(cells, cell_offset, chunk_end);
        // SAFETY: Each chunk row is appended to `new_rows` and returned with
        // the same `new_pages` owner in the enclosing reflow result.
        let mut row = unsafe { Row::new(new_cols, new_pages) };
        copy_cells_to_row(
            &mut row,
            cells,
            cell_offset,
            actual_end,
            new_cols,
            row_u16(new_rows.len()),
            extras_ctx,
        );
        if !first {
            row.set_wrapped(true);
        }
        if let Some(offset) = cursor_offset
            && let Some((r, c)) =
                cursor_in_chunk(offset, cells.len(), cell_offset, actual_end, new_rows.len())
        {
            *cursor_state = (r, c);
        }
        new_rows.push(row);
        cell_offset = actual_end;
        first = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, CellFlags, PageStore, Row};

    // =========================================================================
    // adjust_chunk_boundary
    // =========================================================================

    #[test]
    fn adjust_chunk_boundary_no_wide_chars() {
        // Simple ASCII cells: boundary should remain unchanged.
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
        ];
        assert_eq!(adjust_chunk_boundary(&cells, 0, 2), 2);
        assert_eq!(adjust_chunk_boundary(&cells, 0, 4), 4);
        assert_eq!(adjust_chunk_boundary(&cells, 2, 4), 4);
    }

    #[test]
    fn adjust_chunk_boundary_wide_char_at_boundary() {
        // Wide char at position 1, chunk_end=2 means cells[1] is WIDE.
        // Boundary should back up to 1 to avoid splitting the wide char.
        let cells = [
            Cell::new('A'),
            Cell::with_style(
                'W',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE,
            ),
            Cell::with_style(
                ' ',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE_CONTINUATION,
            ),
            Cell::new('B'),
        ];
        // chunk_end=2, cells[1] is WIDE -> back up to 1
        assert_eq!(adjust_chunk_boundary(&cells, 0, 2), 1);
    }

    #[test]
    fn adjust_chunk_boundary_wide_char_not_at_boundary() {
        // Wide char at position 0, chunk_end=3: cells[2] is WIDE_CONTINUATION (not WIDE).
        // Boundary should remain at 3.
        let cells = [
            Cell::with_style(
                'W',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE,
            ),
            Cell::with_style(
                ' ',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE_CONTINUATION,
            ),
            Cell::new('A'),
        ];
        assert_eq!(adjust_chunk_boundary(&cells, 0, 3), 3);
    }

    #[test]
    fn adjust_chunk_boundary_forward_progress_single_cell() {
        // When actual_end == cell_offset after backup, ensure forward progress.
        // Wide char at position 0, chunk_end=1: cells[0] is WIDE -> back up to 0.
        // But actual_end == cell_offset (0), so force forward to min(2, len).
        let cells = [
            Cell::with_style(
                'W',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE,
            ),
            Cell::with_style(
                ' ',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE_CONTINUATION,
            ),
        ];
        // chunk_end=1, cells[0] is WIDE -> back up to 0 == cell_offset.
        // Forward progress: actual_end = cell_offset + min(2, 2-0) = 2.
        assert_eq!(adjust_chunk_boundary(&cells, 0, 1), 2);
    }

    #[test]
    fn adjust_chunk_boundary_at_end_of_cells() {
        // chunk_end == cells.len(): no cell at boundary to check.
        let cells = [Cell::new('A'), Cell::new('B')];
        assert_eq!(adjust_chunk_boundary(&cells, 0, 2), 2);
    }

    #[test]
    fn adjust_chunk_boundary_single_ascii_cell() {
        let cells = [Cell::new('X')];
        assert_eq!(adjust_chunk_boundary(&cells, 0, 1), 1);
    }

    #[test]
    fn adjust_chunk_boundary_empty_cells() {
        let cells: [Cell; 0] = [];
        // chunk_end=0, cell_offset=0: no adjustment needed.
        assert_eq!(adjust_chunk_boundary(&cells, 0, 0), 0);
    }

    // =========================================================================
    // cursor_in_chunk
    // =========================================================================

    #[test]
    fn cursor_in_chunk_within_range() {
        // cursor at offset 3, chunk [2..5), dest_row=1
        let result = cursor_in_chunk(3, 10, 2, 5, 1);
        assert_eq!(result, Some((1, 1))); // col = 3 - 2 = 1
    }

    #[test]
    fn cursor_in_chunk_at_start() {
        let result = cursor_in_chunk(0, 10, 0, 5, 0);
        assert_eq!(result, Some((0, 0)));
    }

    #[test]
    fn cursor_in_chunk_at_end_non_last() {
        // cursor at offset 5 in chunk [0..5), not last chunk (cells_len=10).
        // 5 is NOT < 5, so not in range.
        let result = cursor_in_chunk(5, 10, 0, 5, 0);
        assert_eq!(result, None);
    }

    #[test]
    fn cursor_in_chunk_at_end_last_chunk() {
        // cursor at offset 10, chunk [5..10), last chunk (cells_len=10).
        // 10 <= 10 (last chunk), so in range.
        let result = cursor_in_chunk(10, 10, 5, 10, 2);
        assert_eq!(result, Some((2, 5))); // col = 10 - 5 = 5
    }

    #[test]
    fn cursor_in_chunk_before_range() {
        let result = cursor_in_chunk(1, 10, 5, 10, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn cursor_in_chunk_after_range() {
        let result = cursor_in_chunk(7, 10, 0, 5, 0);
        assert_eq!(result, None);
    }

    #[test]
    fn cursor_in_chunk_clamped_to_cells_len() {
        // cursor_logical_offset > cells_len gets clamped.
        // clamped = min(100, 10) = 10. chunk [5..10), last chunk.
        // 10 <= 10 (last), so in range.
        let result = cursor_in_chunk(100, 10, 5, 10, 3);
        assert_eq!(result, Some((3, 5)));
    }

    #[test]
    fn cursor_in_chunk_zero_length_cells() {
        // cells_len=0, chunk [0..0), cursor at 0.
        // clamped = 0, is_last_chunk = (0 == 0) = true.
        // in_range = (0 >= 0) && (0 <= 0) = true.
        let result = cursor_in_chunk(0, 0, 0, 0, 0);
        assert_eq!(result, Some((0, 0)));
    }

    // =========================================================================
    // source_coords_for_row
    // =========================================================================

    #[test]
    fn source_coords_for_row_basic() {
        let coords = source_coords_for_row(5, 3);
        assert_eq!(coords.len(), 3);
        assert_eq!(coords[0], CellCoord::new(5, 0));
        assert_eq!(coords[1], CellCoord::new(5, 1));
        assert_eq!(coords[2], CellCoord::new(5, 2));
    }

    #[test]
    fn source_coords_for_row_single_cell() {
        let coords = source_coords_for_row(0, 1);
        assert_eq!(coords.len(), 1);
        assert_eq!(coords[0], CellCoord::new(0, 0));
    }

    #[test]
    fn source_coords_for_row_empty() {
        let coords = source_coords_for_row(10, 0);
        assert!(coords.is_empty());
    }

    #[test]
    fn source_coords_for_row_wide_row() {
        let coords = source_coords_for_row(3, 80);
        assert_eq!(coords.len(), 80);
        assert_eq!(coords[0], CellCoord::new(3, 0));
        assert_eq!(coords[79], CellCoord::new(3, 79));
    }

    // =========================================================================
    // copy_cells_to_row — requires Row allocation
    // =========================================================================

    #[test]
    fn copy_cells_to_row_basic_ascii() {
        let mut pages = PageStore::new();
        let mut new_row = unsafe { Row::new(5, &mut pages) };
        let cells = [Cell::new('A'), Cell::new('B'), Cell::new('C')];
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        copy_cells_to_row(&mut new_row, &cells, 0, 3, 5, 0, &mut extras_ctx);

        assert_eq!(new_row.get(0).unwrap().char_data(), 'A' as u16);
        assert_eq!(new_row.get(1).unwrap().char_data(), 'B' as u16);
        assert_eq!(new_row.get(2).unwrap().char_data(), 'C' as u16);
    }

    #[test]
    fn copy_cells_to_row_with_offset() {
        let mut pages = PageStore::new();
        let mut new_row = unsafe { Row::new(5, &mut pages) };
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
        ];
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        // Copy cells[2..4] = ['C', 'D']
        copy_cells_to_row(&mut new_row, &cells, 2, 4, 5, 0, &mut extras_ctx);

        assert_eq!(new_row.get(0).unwrap().char_data(), 'C' as u16);
        assert_eq!(new_row.get(1).unwrap().char_data(), 'D' as u16);
    }

    #[test]
    fn copy_cells_to_row_wide_char_in_narrow_terminal() {
        // When new_cols < 2, wide chars should be replaced with space.
        let mut pages = PageStore::new();
        let mut new_row = unsafe { Row::new(1, &mut pages) };
        let cells = [
            Cell::with_style(
                'W',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE,
            ),
            Cell::with_style(
                ' ',
                crate::PackedColor::default(),
                crate::PackedColor::default(),
                CellFlags::WIDE_CONTINUATION,
            ),
        ];
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        copy_cells_to_row(&mut new_row, &cells, 0, 2, 1, 0, &mut extras_ctx);

        // Wide char replaced with space, spacer skipped.
        let cell = new_row.get(0).unwrap();
        assert_eq!(cell.char_data(), ' ' as u16);
        assert!(!cell.flags().contains(CellFlags::WIDE));
    }

    #[test]
    fn copy_cells_to_row_empty_range() {
        let mut pages = PageStore::new();
        let mut new_row = unsafe { Row::new(5, &mut pages) };
        let cells = [Cell::new('A')];
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        // cell_offset == actual_end: no cells to copy.
        copy_cells_to_row(&mut new_row, &cells, 0, 0, 5, 0, &mut extras_ctx);
        assert!(new_row.is_empty());
    }

    // =========================================================================
    // chunk_cells_to_rows — requires PageStore/Row allocation
    // =========================================================================

    #[test]
    fn chunk_cells_to_rows_single_chunk() {
        let mut new_pages = PageStore::new();
        let mut new_rows: Vec<Row> = Vec::new();
        let cells = [Cell::new('A'), Cell::new('B'), Cell::new('C')];
        let mut cursor_state = (0usize, 0u16);
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        chunk_cells_to_rows(
            &cells,
            5, // new_cols=5, fits all 3 cells in one row
            &mut new_pages,
            &mut new_rows,
            None,
            &mut cursor_state,
            &mut extras_ctx,
        );

        assert_eq!(new_rows.len(), 1);
        assert_eq!(new_rows[0].get(0).unwrap().char_data(), 'A' as u16);
        assert_eq!(new_rows[0].get(1).unwrap().char_data(), 'B' as u16);
        assert_eq!(new_rows[0].get(2).unwrap().char_data(), 'C' as u16);
        // First row should NOT be wrapped.
        assert!(!new_rows[0].is_wrapped());
    }

    #[test]
    fn chunk_cells_to_rows_multi_chunk() {
        let mut new_pages = PageStore::new();
        let mut new_rows: Vec<Row> = Vec::new();
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
        ];
        let mut cursor_state = (0usize, 0u16);
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        chunk_cells_to_rows(
            &cells,
            2, // new_cols=2: [AB] and [CD]
            &mut new_pages,
            &mut new_rows,
            None,
            &mut cursor_state,
            &mut extras_ctx,
        );

        assert_eq!(new_rows.len(), 2);
        assert_eq!(new_rows[0].get(0).unwrap().char_data(), 'A' as u16);
        assert_eq!(new_rows[0].get(1).unwrap().char_data(), 'B' as u16);
        assert_eq!(new_rows[1].get(0).unwrap().char_data(), 'C' as u16);
        assert_eq!(new_rows[1].get(1).unwrap().char_data(), 'D' as u16);
        // First row NOT wrapped, second IS wrapped.
        assert!(!new_rows[0].is_wrapped());
        assert!(new_rows[1].is_wrapped());
    }

    #[test]
    fn chunk_cells_to_rows_cursor_tracking() {
        let mut new_pages = PageStore::new();
        let mut new_rows: Vec<Row> = Vec::new();
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
            Cell::new('E'),
            Cell::new('F'),
        ];
        let mut cursor_state = (0usize, 0u16);
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        // Cursor at offset 4 ('E'). With new_cols=3: [ABC][DEF].
        // 'E' is at logical offset 4, chunk [3..6), col = 4-3 = 1.
        chunk_cells_to_rows(
            &cells,
            3,
            &mut new_pages,
            &mut new_rows,
            Some(4), // cursor at 'E'
            &mut cursor_state,
            &mut extras_ctx,
        );

        assert_eq!(new_rows.len(), 2);
        assert_eq!(cursor_state, (1, 1)); // row=1, col=1
    }

    #[test]
    fn chunk_cells_to_rows_single_column() {
        let mut new_pages = PageStore::new();
        let mut new_rows: Vec<Row> = Vec::new();
        let cells = [Cell::new('A'), Cell::new('B')];
        let mut cursor_state = (0usize, 0u16);
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        chunk_cells_to_rows(
            &cells,
            1, // new_cols=1: each cell gets its own row
            &mut new_pages,
            &mut new_rows,
            None,
            &mut cursor_state,
            &mut extras_ctx,
        );

        assert_eq!(new_rows.len(), 2);
        assert_eq!(new_rows[0].get(0).unwrap().char_data(), 'A' as u16);
        assert_eq!(new_rows[1].get(0).unwrap().char_data(), 'B' as u16);
        assert!(!new_rows[0].is_wrapped());
        assert!(new_rows[1].is_wrapped());
    }

    #[test]
    fn chunk_cells_to_rows_cursor_on_last_chunk_boundary() {
        let mut new_pages = PageStore::new();
        let mut new_rows: Vec<Row> = Vec::new();
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
        ];
        let mut cursor_state = (0usize, 0u16);
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };

        // Cursor at offset 4 (past end). cells_len=4, last chunk [2..4).
        // clamped = min(4,4) = 4. Last chunk: 4 <= 4 -> in range.
        // Row = 1, col = 4 - 2 = 2.
        chunk_cells_to_rows(
            &cells,
            2,
            &mut new_pages,
            &mut new_rows,
            Some(4),
            &mut cursor_state,
            &mut extras_ctx,
        );

        assert_eq!(new_rows.len(), 2);
        assert_eq!(cursor_state, (1, 2));
    }
}
