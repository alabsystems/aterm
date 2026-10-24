// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reflow chunking, cell copying, and `CellExtras` coordinate remapping.

use crate::CellExtra;
use crate::CellFlags;
#[cfg(any(test, debug_assertions))]
use crate::test_counters::count_reflow_cell_ops;

/// Maps a copied cell's source index back to its [`CellCoord`] during extras remapping.
#[derive(Clone, Copy)]
pub(super) enum ExtrasSource<'a> {
    /// Source row had no extras.
    None,
    /// Single source row `r` where `src_index` maps to `CellCoord::new(r, src_index)`.
    Row(u16),
    /// Explicit index-to-coordinate table across merged source rows.
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
#[inline]
pub(super) const fn adjust_chunk_boundary(
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
        let rem = cells.len() - cell_offset;
        actual_end = cell_offset + if rem < 2 { rem } else { 2 };
    }
    actual_end
}

/// Returns `Some((dest_row, col_in_chunk))` if `cursor_logical_offset` falls within this chunk.
#[inline]
pub(super) const fn cursor_in_chunk(
    cursor_logical_offset: usize,
    cells_len: usize,
    cell_offset: usize,
    actual_end: usize,
    dest_row: usize,
) -> Option<(usize, u16)> {
    let clamped = if cursor_logical_offset < cells_len {
        cursor_logical_offset
    } else {
        cells_len
    };
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

#[inline]
const fn source_coord(extras_ctx: &ExtrasCopyCtx<'_>, src_index: usize) -> Option<CellCoord> {
    match extras_ctx.source {
        ExtrasSource::None => None,
        ExtrasSource::Row(r) => Some(CellCoord::new(r, row_u16(src_index))),
        ExtrasSource::Coords(coords) => {
            if src_index < coords.len() {
                Some(coords[src_index])
            } else {
                None
            }
        }
    }
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
    let Some(coord) = source_coord(extras_ctx, src_index) else {
        return;
    };

    let stored = old_extras.get(coord);
    let complex = if stored.and_then(CellExtra::complex_char).is_none() {
        old_extras.complex_codepoint_for(coord.row, coord.col)
    } else {
        None
    };
    let fg = old_extras.fg_rgb_for(coord.row, coord.col);
    let bg = old_extras.bg_rgb_for(coord.row, coord.col);

    if stored.is_some() || complex.is_some() || fg.is_some() || bg.is_some() {
        let mut extra = stored.cloned().unwrap_or_default();
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

fn is_image_blank(extras_ctx: &ExtrasCopyCtx<'_>, src_index: usize) -> bool {
    let Some(old_extras) = extras_ctx.old_extras else {
        return false;
    };
    source_coord(extras_ctx, src_index)
        .and_then(|coord| old_extras.get(coord))
        .is_some_and(|extra| extra.image().is_some())
}

/// Copies chunk cells into `new_row`, blanking wide glyphs that cannot fit and updating length.
#[allow(clippy::too_many_arguments)]
pub(super) fn copy_cells_to_row(
    new_row: &mut Row,
    cells: &[crate::Cell],
    cell_offset: usize,
    actual_end: usize,
    content_len: usize,
    new_cols: u16,
    dest_row: u16,
    extras_ctx: &mut ExtrasCopyCtx<'_>,
) {
    #[cfg(any(test, debug_assertions))]
    count_reflow_cell_ops(actual_end.saturating_sub(cell_offset));

    let mut j = 0u16;
    let mut content_end = 0u16;
    let mut skip_next_spacer = false;
    for (src_index, cell) in cells[cell_offset..actual_end].iter().enumerate() {
        if cell.flags().contains(CellFlags::WIDE) && new_cols < 2 {
            new_row.set(j, crate::Cell::EMPTY);
            new_row.update_len(j.saturating_add(1));
            j += 1;
            skip_next_spacer = true;
        } else if skip_next_spacer && cell.flags().contains(CellFlags::WIDE_CONTINUATION) {
            skip_next_spacer = false;
        } else if cell.flags().contains(CellFlags::WIDE) && j.saturating_add(1) == new_cols {
            new_row.set(j, crate::Cell::EMPTY);
            j = j.saturating_add(1);
            skip_next_spacer = true;
        } else if j < new_cols {
            let dest_col = j;
            let src = cell_offset + src_index;
            new_row.set(dest_col, *cell);
            remap_copied_extra(extras_ctx, src, dest_row, dest_col);
            j += 1;
            if src < content_len && !(cell.is_empty() && is_image_blank(extras_ctx, src)) {
                content_end = j;
            }
            skip_next_spacer = false;
        } else {
            skip_next_spacer = false;
        }
    }
    new_row.update_len(content_end);
}

/// Chunks `cells` into `new_cols`-wide rows, remapping extras and tracking cursor `points`.
#[allow(clippy::too_many_arguments)]
pub(super) fn chunk_cells_to_rows(
    cells: &[crate::Cell],
    content_len: usize,
    new_cols: u16,
    new_pages: &mut PageStore,
    new_rows: &mut Vec<Row>,
    points: &mut [TrackedPoint],
    extras_ctx: &mut ExtrasCopyCtx<'_>,
) {
    let mut cell_offset = 0;
    let mut first = true;
    while cell_offset < cells.len() {
        let chunk_end = (cell_offset + usize::from(new_cols)).min(cells.len());
        let actual_end = adjust_chunk_boundary(cells, cell_offset, chunk_end);
        // SAFETY: Each chunk row is returned with the same `new_pages` owner.
        let mut row = unsafe { Row::new(new_cols, new_pages) };
        copy_cells_to_row(
            &mut row,
            cells,
            cell_offset,
            actual_end,
            content_len,
            new_cols,
            row_u16(new_rows.len()),
            extras_ctx,
        );
        if !first {
            row.set_wrapped(true);
        }
        if actual_end < chunk_end {
            let col = new_cols - 1;
            let mut blank = crate::Cell::bce_blank(cells[actual_end].colors());
            if blank.colors().bg_is_rgb()
                && let Some(rgb) = extras_ctx.old_extras.and_then(|extras| {
                    let coord = source_coord(extras_ctx, actual_end)?;
                    extras.bg_rgb_for(coord.row, coord.col)
                })
            {
                let mut extra = CellExtra::default();
                extra.set_bg_rgb(Some(rgb));
                extras_ctx
                    .new_extras
                    .set(CellCoord::new(row_u16(new_rows.len()), col), extra);
                blank.set_has_extras(true);
            }
            row.set(col, blank);
            row.set_early_wrap_hole(true);
        }
        for point in points.iter_mut() {
            if let Some(offset) = point.offset
                && let Some((row, col)) =
                    cursor_in_chunk(offset, cells.len(), cell_offset, actual_end, new_rows.len())
            {
                let col = if point.keeps_gap && offset > cells.len() {
                    row_u16(offset.saturating_sub(cell_offset))
                } else {
                    col
                };
                point.at = Some((row, col));
            }
        }
        new_rows.push(row);
        cell_offset = actual_end;
        first = false;
    }
}

/// Cursor logical offset and destination `(row, col)` tracked across line chunking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TrackedPoint {
    pub(super) offset: Option<usize>,
    pub(super) at: Option<(usize, u16)>,
    pub(super) keeps_gap: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, CellFlags, PageStore, Row};

    #[test]
    fn copied_extra_preserves_full_strings_and_resolves_ring_colors() {
        let mut old = CellExtras::new();
        let coord = CellCoord::new(0, 1);
        old.set_complex_char_ring(0, 1, '👩', 2, 8);
        old.set_rgb_ring_range(0, 1, 2, Some([1, 2, 3]), Some([4, 5, 6]), 2, 8);
        let stored = old.get_or_create(coord);
        stored.set_complex_char(Some(std::sync::Arc::from("👩‍💻")));
        stored.set_fg_rgb(Some([90, 91, 92]));
        stored.set_bg_rgb(Some([93, 94, 95]));
        stored.set_hyperlink(Some(std::sync::Arc::from("https://example.com")));
        stored.set_underline_color(Some([7, 8, 9]));
        let mut remapped = CellExtras::new();
        remap_copied_extra(
            &mut ExtrasCopyCtx {
                source: ExtrasSource::Row(0),
                old_extras: Some(&old),
                new_extras: &mut remapped,
            },
            1,
            1,
            3,
        );
        let copied = remapped.get(CellCoord::new(1, 3)).unwrap();
        assert_eq!(copied.complex_char().map(AsRef::as_ref), Some("👩‍💻"));
        assert_eq!(copied.fg_rgb(), Some([1, 2, 3]));
        assert_eq!(copied.bg_rgb(), Some([4, 5, 6]));
        assert_eq!(
            copied.hyperlink().map(AsRef::as_ref),
            Some("https://example.com")
        );
        assert_eq!(copied.underline_color(), Some([7, 8, 9]));
        assert_ne!(old.get(coord).unwrap().fg_rgb(), copied.fg_rgb());
        assert_ne!(old.get(coord).unwrap().bg_rgb(), copied.bg_rgb());
    }

    #[test]
    fn adjust_chunk_boundary_never_splits_a_wide_glyph() {
        let [head, tail] = wide('W');
        let (a, b) = (Cell::new('A'), Cell::new('B'));
        let cases: &[(&str, &[Cell], usize, usize, usize)] = &[
            ("ascii", &[a, b, a, b], 0, 2, 2),
            ("ascii to the end", &[a, b, a, b], 2, 4, 4),
            ("lead at the boundary", &[a, head, tail, b], 0, 2, 1),
            ("continuation at the boundary", &[head, tail, a], 0, 3, 3),
            ("forward progress", &[head, tail], 0, 1, 2),
            ("single cell", &[a], 0, 1, 1),
            ("empty", &[], 0, 0, 0),
        ];
        for &(name, cells, offset, end, want) in cases {
            assert_eq!(adjust_chunk_boundary(cells, offset, end), want, "{name}");
        }
    }

    #[test]
    fn cursor_in_chunk_owns_only_its_span() {
        let cases = [
            (3, 10, 2, 5, 1, Some((1, 1))),
            (0, 10, 0, 5, 0, Some((0, 0))),
            (5, 10, 0, 5, 0, None),
            (10, 10, 5, 10, 2, Some((2, 5))),
            (1, 10, 5, 10, 1, None),
            (7, 10, 0, 5, 0, None),
            (100, 10, 5, 10, 3, Some((3, 5))),
            (0, 0, 0, 0, 0, Some((0, 0))),
        ];
        for (offset, len, start, end, row, want) in cases {
            assert_eq!(
                cursor_in_chunk(offset, len, start, end, row),
                want,
                "offset {offset} of {len} in [{start}, {end})"
            );
        }
    }

    #[test]
    fn copy_cells_to_row_wide_char_in_narrow_terminal() {
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

        copy_cells_to_row(
            &mut new_row,
            &cells,
            0,
            2,
            cells.len(),
            1,
            0,
            &mut extras_ctx,
        );

        let cell = new_row.get(0).unwrap();
        assert_eq!(cell.char_data(), ' ' as u16);
        assert!(!cell.flags().contains(CellFlags::WIDE));
    }

    type Chunked = (PageStore, Vec<Row>, [Option<(usize, u16)>; 2]);

    fn chunk(cells: &[Cell], new_cols: u16, offsets: [Option<usize>; 2]) -> Chunked {
        let mut new_pages = PageStore::new();
        let mut new_rows: Vec<Row> = Vec::new();
        let mut new_extras = CellExtras::new();
        let mut extras_ctx = ExtrasCopyCtx {
            source: ExtrasSource::None,
            old_extras: None,
            new_extras: &mut new_extras,
        };
        let mut points = offsets.map(|offset| TrackedPoint {
            offset,
            at: None,
            keeps_gap: false,
        });
        chunk_cells_to_rows(
            cells,
            cells.len(),
            new_cols,
            &mut new_pages,
            &mut new_rows,
            &mut points,
            &mut extras_ctx,
        );
        (new_pages, new_rows, points.map(|p| p.at))
    }

    fn wide(c: char) -> [Cell; 2] {
        [
            Cell::with_style(
                c,
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
        ]
    }

    #[test]
    fn chunk_cells_to_rows_multi_chunk() {
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
        ];
        let (_pages, new_rows, _) = chunk(&cells, 2, [None, None]);
        assert_eq!(new_rows.len(), 2);
        assert_eq!(new_rows[0].get(0).unwrap().char_data(), 'A' as u16);
        assert_eq!(new_rows[0].get(1).unwrap().char_data(), 'B' as u16);
        assert_eq!(new_rows[1].get(0).unwrap().char_data(), 'C' as u16);
        assert_eq!(new_rows[1].get(1).unwrap().char_data(), 'D' as u16);
        assert!(!new_rows[0].is_wrapped());
        assert!(new_rows[1].is_wrapped());
    }

    #[test]
    fn chunk_cells_to_rows_tracks_two_cursors() {
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
            Cell::new('E'),
            Cell::new('F'),
        ];
        let (_pages, new_rows, at) = chunk(&cells, 3, [Some(4), Some(1)]);
        assert_eq!(new_rows.len(), 2);
        assert_eq!(at, [Some((1, 1)), Some((0, 1))]);
    }

    #[test]
    fn chunk_cells_to_rows_cursor_on_last_chunk_boundary() {
        let cells = [
            Cell::new('A'),
            Cell::new('B'),
            Cell::new('C'),
            Cell::new('D'),
        ];
        let (_pages, new_rows, at) = chunk(&cells, 2, [Some(4), None]);
        assert_eq!(new_rows.len(), 2);
        assert_eq!(at, [Some((1, 2)), None]);
    }

    #[test]
    fn a_printed_space_at_a_chunk_end_stays_inside_the_rows_length() {
        let cells = [
            Cell::new('a'),
            Cell::new('b'),
            Cell::new(' '),
            Cell::new('c'),
        ];
        let (_pages, new_rows, _) = chunk(&cells, 3, [None, None]);
        assert_eq!(new_rows[0].len(), 3, "`ab ` keeps its space");
        assert_eq!(new_rows[1].len(), 1);
    }

    #[test]
    fn a_chunk_backed_off_a_wide_glyph_records_its_hole() {
        let [head, tail] = wide('中');
        let cells = [Cell::new('a'), Cell::new('b'), head, tail, Cell::new('c')];
        let (_pages, new_rows, _) = chunk(&cells, 3, [None, None]);
        assert_eq!(new_rows.len(), 2);
        assert!(new_rows[0].has_early_wrap_hole(), "`ab` + hole");
        assert!(!new_rows[1].has_early_wrap_hole(), "`中c` fills its row");

        let cells = [Cell::new('a'), Cell::new('b'), Cell::new(' '), head, tail];
        let (_pages, new_rows, _) = chunk(&cells, 3, [None, None]);
        assert!(
            !new_rows[0].has_early_wrap_hole(),
            "`ab ` is full: its last cell is a printed space, not a hole"
        );
    }
}
