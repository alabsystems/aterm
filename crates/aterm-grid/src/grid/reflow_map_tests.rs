// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
        Cell::from_raw_parts(
            0,
            crate::PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg(),
            CellFlags::COMPLEX,
        ),
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

/// A cell written over ring slots keeps none of them through the copy unless its bits
/// claim them: an indexed or default colour, or a BMP glyph, takes no slot's value.
#[test]
fn copied_extra_takes_only_the_ring_slots_the_cell_claims() {
    let mut old = CellExtras::new();
    old.set_complex_char_ring(0, 1, '👩', 2, 8);
    old.set_rgb_ring_range(0, 1, 2, Some([1, 2, 3]), Some([4, 5, 6]), 2, 8);
    let indexed = crate::PackedColors::DEFAULT.set_fg_indexed(1);
    let cases = [
        ("plain", Cell::new('A'), None),
        (
            "fg only",
            Cell::from_raw_parts(u16::from(b'A'), indexed.with_rgb_fg(), CellFlags::empty()),
            Some((Some([1, 2, 3]), None, false)),
        ),
        (
            "bg only",
            Cell::from_raw_parts(u16::from(b'A'), indexed.with_rgb_bg(), CellFlags::empty()),
            Some((None, Some([4, 5, 6]), false)),
        ),
        (
            "complex only",
            Cell::from_raw_parts(0, indexed, CellFlags::COMPLEX),
            Some((None, None, true)),
        ),
    ];
    for (name, cell, want) in cases {
        let mut remapped = CellExtras::new();
        remap_copied_extra(
            &mut ExtrasCopyCtx {
                source: ExtrasSource::Row(0),
                old_extras: Some(&old),
                new_extras: &mut remapped,
            },
            cell,
            1,
            1,
            3,
        );
        let got = remapped.get(CellCoord::new(1, 3)).map(|extra| {
            (
                extra.fg_rgb(),
                extra.bg_rgb(),
                extra.complex_char().is_some(),
            )
        });
        assert_eq!(got, want, "{name}");
    }
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
        opens_row: false,
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
