// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The written window (`written_lo..written_hi`) covers every cell a row method writes: after
//! each step of generated writes, wide runs, fills, range clears, character shifts and resizes,
//! every cell outside it is empty, and an erase leaves the row exactly as filling every cell does.

use aterm_spec::rng::XorShift;

use super::*;
use crate::{CellFlags, PackedColors};

fn outside_window_is_empty(row: &Row) -> bool {
    let hi = usize::from(row.written_hi).min(row.cells.len());
    let lo = usize::from(row.written_lo).min(hi);
    row.cells[..lo]
        .iter()
        .chain(&row.cells[hi..])
        .all(|cell| *cell == Cell::EMPTY)
}

fn glyph(ch: u8, colors: PackedColors) -> Cell {
    Cell::from_raw_parts(u16::from(ch), colors, CellFlags::empty())
}

#[test]
fn every_row_write_stays_inside_the_written_window() {
    let mut rng = XorShift::new(0x3B1A_7C0F_F1E5);
    let palette = [
        PackedColors::DEFAULT,
        PackedColors::with_indexed_bg(3),
        PackedColors::new().with_rgb_bg(),
    ];
    let mut pages = PageStore::new();
    let (mut erases, mut partial) = (0usize, 0usize);
    for _ in 0..400 {
        let cols = u16::try_from(1 + rng.below(12)).unwrap_or(1);
        // SAFETY: the row never outlives `pages`, which this test owns.
        let mut row = unsafe { Row::new(cols, &mut pages) };
        for step in 0..60 {
            let cols = row.cols();
            let col = u16::try_from(rng.below(u64::from(cols) + 1)).unwrap_or(0);
            let count = u16::try_from(rng.below(u64::from(cols) + 2)).unwrap_or(0);
            let colors = *rng.pick(&palette);
            let fill = Cell::bce_blank(colors);
            match rng.below(15) {
                0 => {
                    row.write_char(col, *rng.pick(&['a', 'Z', ' ', '中']));
                }
                1 => {
                    row.write_char_packed(col, 'q', colors, CellFlags::empty());
                }
                2 => {
                    row.write_wide_char_packed(col, '中', colors, CellFlags::empty());
                }
                3 => {
                    row.fill_cell_run(col, count, glyph(b'x', colors));
                }
                4 => {
                    if let Some(span) = row.cells_mut_with_fixup(col, count, fill) {
                        span.fill(glyph(b'm', colors));
                    }
                }
                5 => {
                    row.set(col, glyph(b's', colors));
                }
                6 => {
                    if let Some(cell) = row.get_mut(col) {
                        *cell = glyph(b'g', colors);
                    }
                }
                7 => row.clear_range_with(col, col.saturating_add(count), fill),
                8 => {
                    row.insert_chars_fill(col, count, fill);
                }
                9 => {
                    row.delete_chars_fill(col, count, fill);
                }
                10 => {
                    row.erase_chars_with(col, count, fill);
                }
                11 => {
                    let cols = u16::try_from(1 + rng.below(12)).unwrap_or(1);
                    // SAFETY: as for `Row::new` above.
                    unsafe { row.resize(cols, &mut pages) };
                }
                12 => {
                    // A wide run as `Grid::write_wide_run_autowrap` writes one: the fixup, the
                    // glyphs with no window note of their own, then the settle that notes the run.
                    let pairs = (cols.saturating_sub(col) / 2).min(count);
                    if pairs > 0 {
                        let end = col + 2 * pairs;
                        let blanked = row.fixup_wide_chars_in_range(col, 2 * pairs, fill);
                        let glyphs = vec!['中'; usize::from(pairs)];
                        row.write_wide_run_packed_no_fixup(col, &glyphs, colors, |_| {
                            CellFlags::empty()
                        });
                        row.settle_len_after_wide_run(col, end, blanked);
                        row.mark_has_wide_chars();
                    }
                }
                _ => {
                    let fill = if rng.below(2) == 0 { Cell::EMPTY } else { fill };
                    let mut want = row.cells.to_vec();
                    want.fill(fill);
                    let window = usize::from(row.written_hi.saturating_sub(row.written_lo));
                    partial += usize::from(fill == Cell::EMPTY && window < want.len());
                    match rng.below(3) {
                        0 => row.erase_with(fill),
                        1 => row.reset_with(fill),
                        _ => {
                            row.clear();
                            want.fill(Cell::EMPTY);
                        }
                    }
                    assert_eq!(row.cells.as_slice(), &want[..], "erase at step {step}");
                    erases += 1;
                }
            }
            assert!(
                outside_window_is_empty(&row),
                "step {step}: a cell outside {}..{} is not empty",
                row.written_lo,
                row.written_hi
            );
        }
    }
    assert!(
        erases > 2_000 && partial > 500,
        "erases {erases}, of them narrower than the row {partial}"
    );
}
