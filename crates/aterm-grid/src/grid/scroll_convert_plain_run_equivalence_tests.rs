// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn old_cell_loop(cells: &[Cell], wrapped: bool) -> Line {
    let mut text = String::new();
    let mut attrs = AttrRunBuilder::empty();
    for (idx, cell) in cells.iter().enumerate() {
        if is_spacer(cells, idx) {
            continue;
        }
        text.push(cell.char());
        let fg = cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0);
        let bg = cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0);
        attrs.push(CellAttrs::from_raw(fg, bg, cell.flags().bits()));
    }
    let mut line = Line::with_hyperlinks_owned(text, attrs.finish(), Vec::new());
    line.set_wrapped(wrapped);
    line
}

fn equivalent(actual: &Line, expected: &Line) -> bool {
    actual.to_string() == expected.to_string()
        && actual.attrs().map(Rle::runs) == expected.attrs().map(Rle::runs)
        && actual.is_wrapped() == expected.is_wrapped()
        && aterm_scrollback::serialize_lines(std::slice::from_ref(actual))
            == aterm_scrollback::serialize_lines(std::slice::from_ref(expected))
}

fn check_both_paths(cells: &[Cell], wrapped: bool) {
    let len = u16::try_from(cells.len()).expect("bounded fixture");
    let mut grid = Grid::new(1, len.max(1));
    let row = grid.row_mut(0).expect("fixture row");
    for (col, cell) in cells.iter().enumerate() {
        assert!(row.set(u16::try_from(col).expect("bounded column"), *cell));
    }
    row.update_len(len);
    row.set_wrapped(wrapped);
    let expected = old_cell_loop(cells, wrapped);
    let before = before_neon(cells);
    let eager =
        Grid::row_to_line_with_stored_extras_at_len(row, &ScrolledRowExtras::default(), len);
    assert!(
        equivalent(&eager, &expected),
        "eager len={len}, wrapped={wrapped}"
    );
    assert_eq!(eager.memory_used(), before.memory_used());
    let mut staged = super::super::lazy_buffer::LazyBuffer::new();
    staged.push_row(row, ScrolledRowExtras::default());
    staged.push_row(row, ScrolledRowExtras::default());
    let read = staged.get_line(0).expect("the staged row");
    assert!(
        equivalent(read, &expected),
        "staged len={len}, wrapped={wrapped}"
    );
    assert_eq!(read.memory_used(), before.memory_used());
    let drained: Vec<Line> = staged.drain_all().collect();
    for consumed in &drained {
        assert!(
            equivalent(consumed, &expected),
            "drained len={len}, wrapped={wrapped}"
        );
        assert_eq!(consumed.memory_used(), before.memory_used());
    }
}

fn styled(ch: char, color: u8, flags: CellFlags) -> Cell {
    Cell::with_style(
        ch,
        PackedColor::indexed(color),
        PackedColor::DEFAULT_BG,
        flags,
    )
}

#[test]
fn plain_run_materialization_matches_old_loop_at_chunk_boundaries() {
    for len in [0, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128, 129] {
        for wrapped in [false, true] {
            check_both_paths(&vec![Cell::new('x'); len], wrapped);
            check_both_paths(&vec![styled('x', 2, CellFlags::BOLD); len], wrapped);
            check_both_paths(&vec![Cell::EMPTY; len], wrapped);
            let alternating: Vec<_> = (0..len)
                .map(|i| styled('x', if i % 2 == 0 { 1 } else { 2 }, CellFlags::BOLD))
                .collect();
            check_both_paths(&alternating, wrapped);
        }
    }
    for boundary in [7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128] {
        let mut cells = vec![styled('a', 1, CellFlags::BOLD); 129];
        cells[boundary..].fill(styled('b', 2, CellFlags::ITALIC));
        check_both_paths(&cells, false);
        check_both_paths(&cells, true);
    }
}

#[test]
fn plain_run_materialization_matches_old_loop_across_fallbacks() {
    let fallback = [
        Cell::new('\0'),
        Cell::new('\u{1f}'),
        Cell::new('\u{7f}'),
        Cell::new('\u{80}'),
        Cell::new('é'),
        styled('世', 2, CellFlags::WIDE),
        styled(' ', 2, CellFlags::WIDE_CONTINUATION),
        styled('!', 2, CellFlags::PROTECTED),
        styled('x', 2, CellFlags::COMPLEX),
        Cell::from_raw_parts(0xD800, crate::PackedColors::DEFAULT, CellFlags::empty()),
    ];
    for boundary in [0, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128] {
        let mut cells = vec![Cell::new('a'); boundary];
        cells.extend_from_slice(&fallback);
        cells.extend(std::iter::repeat_n(Cell::new('z'), 65));
        check_both_paths(&cells, false);
        check_both_paths(&cells, true);
    }
    let expected = old_cell_loop(&fallback, false);
    assert_eq!(
        expected.to_string(),
        "\0\u{1f}\u{7f}\u{80}é世!\u{fffd}\u{fffd}"
    );
}

#[test]
fn every_block_lane_preserves_the_first_nonmatching_cell() {
    let exceptions = [
        styled('\u{1f}', 2, CellFlags::BOLD),
        styled('\u{7f}', 2, CellFlags::BOLD),
        styled('\u{80}', 2, CellFlags::BOLD),
        styled('é', 2, CellFlags::BOLD),
        styled('x', 3, CellFlags::BOLD),
        styled('x', 2, CellFlags::BOLD | CellFlags::ITALIC),
        Cell::with_style(
            'x',
            PackedColor::indexed(2),
            PackedColor::indexed(3),
            CellFlags::BOLD,
        ),
        styled('世', 2, CellFlags::BOLD | CellFlags::WIDE),
        styled(' ', 2, CellFlags::BOLD | CellFlags::WIDE_CONTINUATION),
        styled('p', 2, CellFlags::BOLD | CellFlags::PROTECTED),
        styled('x', 2, CellFlags::BOLD | CellFlags::COMPLEX),
        Cell::from_raw_parts(
            0xD800,
            Cell::convert_colors(PackedColor::indexed(2), PackedColor::DEFAULT_BG),
            CellFlags::BOLD,
        ),
    ];
    for position in 0..137 {
        for exception in exceptions {
            let mut cells: Vec<_> = (0..137)
                .map(|column| {
                    styled(
                        char::from(32 + ((column * 37 + position * 13) % 95) as u8),
                        2,
                        CellFlags::BOLD,
                    )
                })
                .collect();
            cells[position] = exception;
            check_both_paths(&cells, false);
            check_both_paths(&cells, true);
        }
    }
}

#[repr(C, align(2))]
struct AlignedCells<const N: usize>([Cell; N]);

#[repr(C, align(2))]
struct OddCells<const N: usize> {
    prefix: u8,
    cells: [Cell; N],
}

#[test]
fn block_checks_every_character_color_and_flag_lane() {
    let flags = CellFlags::BOLD;
    let colors = Cell::convert_colors(PackedColor::indexed(2), PackedColor::DEFAULT_BG);
    let original =
        std::array::from_fn(|i| Cell::from_raw_parts(u16::from(b'A') + i as u16, colors, flags));
    let check = |cells: [Cell; 8], expected: bool| {
        let aligned = AlignedCells(cells);
        let odd = OddCells { prefix: 0, cells };
        assert!(aligned.0.as_ptr().cast::<u16>().is_aligned());
        assert!(!odd.cells.as_ptr().cast::<u16>().is_aligned());
        for block in [&aligned.0, &odd.cells] {
            let mut out = [0; 8];
            assert_eq!(
                copy_plain_ascii_block(block, flags.bits(), colors, &mut out),
                expected
            );
            if expected {
                assert_eq!(out, cells.map(|cell| cell.char_data() as u8));
            }
        }
    };
    for lane in 0..8 {
        for raw in [0, 31, 32, 126, 127, 128, 255, 256, 0xd800, u16::MAX] {
            let mut cells = AlignedCells(original);
            cells.0[lane] = Cell::from_raw_parts(raw, colors, flags);
            check(cells.0, (32..=127).contains(&raw));
        }
        for bit in 0..32 {
            let mut cells = AlignedCells(original);
            cells.0[lane] = Cell::from_raw_parts(
                u16::from(b'A') + lane as u16,
                crate::PackedColors(colors.0 ^ (1 << bit)),
                flags,
            );
            check(cells.0, false);
        }
        for bit in 0..16 {
            let mut cells = AlignedCells(original);
            cells.0[lane] = Cell::from_raw_parts(
                u16::from(b'A') + lane as u16,
                colors,
                CellFlags(flags.bits() ^ (1 << bit)),
            );
            check(cells.0, false);
        }
    }
}

#[test]
fn odd_aligned_cells_use_portable_block_and_conversion() {
    let mut odd = OddCells {
        prefix: 0,
        cells: std::array::from_fn::<_, 137, _>(|i| {
            styled(char::from(32 + ((i * 37) % 96) as u8), 2, CellFlags::BOLD)
        }),
    };
    assert!(!odd.cells.as_ptr().cast::<u16>().is_aligned());
    for position in [0, 7, 8, 9, 63, 64, 65, 127, 128, 136] {
        let original = odd.cells[position];
        for replacement in [
            styled('é', 2, CellFlags::BOLD),
            styled('x', 3, CellFlags::BOLD),
        ] {
            odd.cells[position] = replacement;
            for wrapped in [false, true] {
                let expected = old_cell_loop(&odd.cells, wrapped);
                let mut actual = plain_cells_to_line(&odd.cells);
                actual.set_wrapped(wrapped);
                assert!(equivalent(&actual, &expected), "position={position}");
                assert_eq!(actual.memory_used(), before_neon(&odd.cells).memory_used());
            }
        }
        odd.cells[position] = original;
    }
    let block: &[Cell; 8] = (&odd.cells[..8]).try_into().expect("eight cells");
    let mut out = [0; 8];
    assert!(copy_plain_ascii_block(
        block,
        block[0].flags().bits(),
        block[0].colors(),
        &mut out
    ));
    assert_eq!(out, std::array::from_fn(|i| block[i].char_data() as u8));
}

#[inline]
fn copy_plain_ascii_block_scalar(
    cells: &[Cell; 8],
    flags: u16,
    colors: crate::PackedColors,
    out: &mut [u8; 8],
) -> bool {
    let mut mismatch = 0u32;
    for (offset, cell) in cells.iter().enumerate() {
        let raw = cell.char_data();
        mismatch |= u32::from(cell.flags().bits() ^ flags);
        mismatch |= cell.colors().0 ^ colors.0;
        mismatch |= u32::from(raw.wrapping_sub(0x20) >= 0x60);
        out[offset] = (raw & 0xff) as u8;
    }
    mismatch == 0
}

#[inline]
fn append_current_scalar<const CHECK_UTF8: bool>(
    cells: &[Cell],
    from: usize,
    text: &mut String,
    attrs_rle: &mut AttrRunBuilder,
) -> usize {
    let Some(first) = plain_ascii_byte(&cells[from]) else {
        return from;
    };
    let attrs = cell_attrs_of(&cells[from]);
    let flags = cells[from].flags().bits();
    let colors = cells[from].colors();
    if cells
        .get(from + 1)
        .is_some_and(|next| next.flags().bits() != flags || next.colors() != colors)
    {
        text.push(char::from(first));
        attrs_rle.extend(attrs, 1);
        return from + 1;
    }

    let mut chunk = [0u8; 64];
    let mut end = from;
    while end < cells.len() {
        let mut n = 0usize;
        while n + 8 <= chunk.len() && end + 8 <= cells.len() {
            let block = cells[end..end + 8].try_into().expect("eight cells");
            let out = (&mut chunk[n..n + 8]).try_into().expect("eight bytes");
            if !copy_plain_ascii_block_scalar(block, flags, colors, out) {
                break;
            }
            end += 8;
            n += 8;
        }

        while n < chunk.len() && end < cells.len() {
            let cell = &cells[end];
            if cell.flags().bits() != flags || cell.colors() != colors {
                break;
            }
            let Some(byte) = plain_ascii_byte(cell) else {
                break;
            };
            chunk[n] = byte;
            n += 1;
            end += 1;
        }
        if n == 0 {
            break;
        }
        let ascii = if CHECK_UTF8 {
            std::str::from_utf8(&chunk[..n]).unwrap_or_default()
        } else {
            // SAFETY: Block lookahead writes before validation, but n advances
            // only when every raw character passes 0x20..0x80. On failure the
            // scalar tail overwrites its accepted prefix with plain_ascii_byte
            // results before advancing n; all other lookahead stays outside
            // this slice. Thus every selected byte is initialized ASCII.
            unsafe { std::str::from_utf8_unchecked(&chunk[..n]) }
        };
        text.push_str(ascii);
        if n < chunk.len() {
            break;
        }
    }
    attrs_rle.extend(attrs, u32::try_from(end - from).unwrap_or(u32::MAX));
    end
}

#[inline]
fn current_scalar(cells: &[Cell]) -> Line {
    let mut text = String::with_capacity(cells.len());
    let mut attrs_rle = AttrRunBuilder::empty();

    let mut idx = 0;
    while idx < cells.len() {
        let run_end = append_current_scalar::<false>(cells, idx, &mut text, &mut attrs_rle);
        if run_end > idx {
            idx = run_end;
            continue;
        }
        let cell = &cells[idx];
        if !is_spacer(cells, idx) {
            text.push(cell.char());
            attrs_rle.push(cell_attrs_of(cell));
        }
        idx += 1;
    }

    Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new())
}

// Frozen BACC scalar eight-cell implementation; the older before_blocks
// below remains the historical 5c307 comparison only.
#[inline]
fn append_before_neon<const CHECK_UTF8: bool>(
    cells: &[Cell],
    from: usize,
    text: &mut String,
    attrs_rle: &mut AttrRunBuilder,
) -> usize {
    let Some(first) = plain_ascii_byte(&cells[from]) else {
        return from;
    };
    let attrs = cell_attrs_of(&cells[from]);
    let flags = cells[from].flags().bits();
    let colors = cells[from].colors();
    if cells
        .get(from + 1)
        .is_some_and(|next| next.flags().bits() != flags || next.colors() != colors)
    {
        text.push(char::from(first));
        attrs_rle.extend(attrs, 1);
        return from + 1;
    }

    let mut chunk = [0u8; 64];
    let mut end = from;
    while end < cells.len() {
        let mut n = 0usize;
        while n + 8 <= chunk.len() && end + 8 <= cells.len() {
            let mut mismatch = 0u32;
            for offset in 0..8 {
                let cell = &cells[end + offset];
                let raw = cell.char_data();
                mismatch |= u32::from(cell.flags().bits() ^ flags);
                mismatch |= cell.colors().0 ^ colors.0;
                mismatch |= u32::from(raw.wrapping_sub(0x20) >= 0x60);
                chunk[n + offset] = (raw & 0xff) as u8;
            }
            if mismatch != 0 {
                break;
            }
            end += 8;
            n += 8;
        }

        while n < chunk.len() && end < cells.len() {
            let cell = &cells[end];
            if cell.flags().bits() != flags || cell.colors() != colors {
                break;
            }
            let Some(byte) = plain_ascii_byte(cell) else {
                break;
            };
            chunk[n] = byte;
            n += 1;
            end += 1;
        }
        if n == 0 {
            break;
        }
        let ascii = if CHECK_UTF8 {
            std::str::from_utf8(&chunk[..n]).unwrap_or_default()
        } else {
            // SAFETY: Block lookahead writes before validation, but n advances
            // only when every raw character passes 0x20..0x80. On failure the
            // scalar tail overwrites its accepted prefix with plain_ascii_byte
            // results before advancing n; all other lookahead stays outside
            // this slice. Thus every selected byte is initialized ASCII.
            unsafe { std::str::from_utf8_unchecked(&chunk[..n]) }
        };
        text.push_str(ascii);
        if n < chunk.len() {
            break;
        }
    }
    attrs_rle.extend(attrs, u32::try_from(end - from).unwrap_or(u32::MAX));
    end
}

#[inline]
fn before_neon(cells: &[Cell]) -> Line {
    let mut text = String::with_capacity(cells.len());
    let mut attrs_rle = AttrRunBuilder::empty();

    let mut idx = 0;
    while idx < cells.len() {
        let run_end = append_before_neon::<false>(cells, idx, &mut text, &mut attrs_rle);
        if run_end > idx {
            idx = run_end;
            continue;
        }
        let cell = &cells[idx];
        if !is_spacer(cells, idx) {
            text.push(cell.char());
            attrs_rle.push(cell_attrs_of(cell));
        }
        idx += 1;
    }

    Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new())
}

// Frozen pre-block converter for this manual before/after measurement. The
// correctness oracle above is deliberately the separate per-cell loop.
fn before_blocks(cells: &[Cell]) -> Line {
    #[inline]
    fn append(
        cells: &[Cell],
        from: usize,
        text: &mut String,
        attrs_rle: &mut AttrRunBuilder,
    ) -> usize {
        let Some(first) = plain_ascii_byte(&cells[from]) else {
            return from;
        };
        let attrs = cell_attrs_of(&cells[from]);
        let flags = cells[from].flags().bits();
        let colors = cells[from].colors();
        let mut chunk = [0u8; 64];
        chunk[0] = first;
        let mut n = 1usize;
        let mut end = from + 1;
        while end < cells.len() {
            let cell = &cells[end];
            if cell.flags().bits() != flags || cell.colors() != colors {
                break;
            }
            let Some(byte) = plain_ascii_byte(cell) else {
                break;
            };
            if n == chunk.len() {
                text.push_str(std::str::from_utf8(&chunk).unwrap_or_default());
                n = 0;
            }
            chunk[n] = byte;
            n += 1;
            end += 1;
        }
        text.push_str(std::str::from_utf8(&chunk[..n]).unwrap_or_default());
        attrs_rle.extend(attrs, u32::try_from(end - from).unwrap_or(u32::MAX));
        end
    }

    let mut text = String::with_capacity(cells.len());
    let mut attrs = AttrRunBuilder::empty();
    let mut at = 0;
    while at < cells.len() {
        let end = append(cells, at, &mut text, &mut attrs);
        if end > at {
            at = end;
        } else {
            if !is_spacer(cells, at) {
                text.push(cells[at].char());
                attrs.push(cell_attrs_of(&cells[at]));
            }
            at += 1;
        }
    }
    Line::with_hyperlinks_owned(text, attrs.finish(), Vec::new())
}

#[test]
#[ignore = "manual optimized comparison; run with --profile opt-test --ignored --nocapture"]
fn compare_plain_conversion() {
    use std::hint::black_box;
    use std::time::Instant;

    fn measure(convert: fn(&[Cell]) -> Line, rows: &[Vec<Cell>]) -> (u128, usize) {
        let convert = black_box(convert);
        let start = Instant::now();
        let mut checksum = 0usize;
        for i in 0..250_000usize {
            let line = convert(black_box(&rows[i % rows.len()]));
            checksum = checksum.wrapping_add(line.len());
            black_box(line);
        }
        (start.elapsed().as_nanos(), black_box(checksum))
    }

    for (name, width, kind) in [
        ("chain79", 79, 0),
        ("chain127", 127, 0),
        ("plain80", 80, 1),
        ("unicode80", 80, 2),
        ("stylebreak80", 80, 3),
    ] {
        let rows: Vec<Vec<Cell>> = (0..128)
            .map(|row| {
                (0..width)
                    .map(|column| {
                        let ch = if kind == 2 && column % 17 == 0 {
                            'é'
                        } else {
                            char::from(b' ' + ((row * 7 + column) % 95) as u8)
                        };
                        if kind == 3 {
                            styled(ch, 1 + (column % 6) as u8, CellFlags::empty())
                        } else if kind == 0 && column < 8 {
                            styled(ch, 1 + (row % 6) as u8, CellFlags::empty())
                        } else {
                            Cell::new(ch)
                        }
                    })
                    .collect()
            })
            .collect();
        for row in &rows {
            assert!(equivalent(&before_blocks(row), &plain_cells_to_line(row)));
        }
        let mut before = Vec::new();
        let mut checked = Vec::new();
        let mut unchecked = Vec::new();
        let expected_bytes = before_blocks(&rows[0]).len() * 250_000;
        for _ in 0..5 {
            for variant in [0, 1, 2, 2, 1, 0] {
                let convert = match variant {
                    0 => before_blocks,
                    1 => plain_cells_to_line_with::<true>,
                    _ => plain_cells_to_line,
                };
                let (elapsed, checksum) = measure(convert, &rows);
                assert_eq!(checksum, expected_bytes);
                match variant {
                    0 => &mut before,
                    1 => &mut checked,
                    _ => &mut unchecked,
                }
                .push(elapsed);
            }
        }
        let median = |samples: &[u128]| {
            let mut sorted = samples.to_vec();
            sorted.sort_unstable();
            sorted[(sorted.len() - 1) / 2]
        };
        let before_median = median(&before);
        let checked_median = median(&checked);
        let unchecked_median = median(&unchecked);
        eprintln!(
            "plain-conversion {name} rows=250000 pairs=5 order=ABCCBA before_ns={before:?} checked_ns={checked:?} unchecked_ns={unchecked:?} before_median_ns={before_median} checked_median_ns={checked_median} unchecked_median_ns={unchecked_median} old_over_checked_milli={} old_over_unchecked_milli={} checked_over_unchecked_milli={}",
            before_median * 1000 / checked_median,
            before_median * 1000 / unchecked_median,
            checked_median * 1000 / unchecked_median,
        );
    }
}

#[test]
#[ignore = "manual BACC versus NEON comparison; run with --profile opt-test --ignored --nocapture"]
fn compare_neon_conversion() {
    use std::hint::black_box;
    use std::time::Instant;

    fn measure(convert: fn(&[Cell]) -> Line, rows: &[Vec<Cell>]) -> (u128, usize) {
        let convert = black_box(convert);
        let start = Instant::now();
        let mut checksum = 0usize;
        for i in 0..250_000usize {
            let line = convert(black_box(&rows[i % rows.len()]));
            checksum = checksum.wrapping_add(line.len());
            black_box(line);
        }
        (start.elapsed().as_nanos(), black_box(checksum))
    }

    for (name, width, kind) in [
        ("chain79", 79, 0),
        ("chain127", 127, 0),
        ("chain511", 511, 0),
        ("plain80", 80, 1),
        ("unicode80", 80, 2),
        ("stylebreak80", 80, 3),
    ] {
        let rows: Vec<Vec<Cell>> = (0..128)
            .map(|row| {
                (0..width)
                    .map(|column| {
                        let ch = if kind == 2 && column % 17 == 0 {
                            'é'
                        } else {
                            char::from(b' ' + ((row * 7 + column) % 95) as u8)
                        };
                        if kind == 3 {
                            styled(ch, 1 + (column % 6) as u8, CellFlags::empty())
                        } else if kind == 0 && column < 8 {
                            styled(ch, 1 + (row % 6) as u8, CellFlags::empty())
                        } else {
                            Cell::new(ch)
                        }
                    })
                    .collect()
            })
            .collect();
        for row in &rows {
            let before = before_neon(row);
            let scalar = current_scalar(row);
            let after = plain_cells_to_line(row);
            assert!(equivalent(&before, &scalar));
            assert_eq!(before.memory_used(), scalar.memory_used());
            assert!(equivalent(&before, &after));
            assert_eq!(before.memory_used(), after.memory_used());
        }
        let mut before = Vec::new();
        let mut scalar = Vec::new();
        let mut after = Vec::new();
        let expected_bytes = before_neon(&rows[0]).len() * 250_000;
        for _ in 0..5 {
            for variant in [0, 1, 2, 2, 1, 0] {
                let convert = match variant {
                    0 => before_neon,
                    1 => current_scalar,
                    _ => plain_cells_to_line,
                };
                let (elapsed, checksum) = measure(convert, &rows);
                assert_eq!(checksum, expected_bytes);
                match variant {
                    0 => &mut before,
                    1 => &mut scalar,
                    _ => &mut after,
                }
                .push(elapsed);
            }
        }
        let median = |samples: &[u128]| {
            let mut sorted = samples.to_vec();
            sorted.sort_unstable();
            sorted[(sorted.len() - 1) / 2]
        };
        let before_median = median(&before);
        let scalar_median = median(&scalar);
        let after_median = median(&after);
        eprintln!(
            "neon-conversion {name} rows=250000 pairs=5 order=ABCCBA bacc_ns={before:?} scalar_ns={scalar:?} neon_ns={after:?} bacc_median_ns={before_median} scalar_median_ns={scalar_median} neon_median_ns={after_median} bacc_over_neon_milli={} bacc_over_scalar_milli={} scalar_over_neon_milli={}",
            before_median * 1000 / after_median,
            before_median * 1000 / scalar_median,
            scalar_median * 1000 / after_median,
        );
    }
}

#[test]
fn plain_run_raw_color_boundaries_preserve_semantic_attribute_runs() {
    // Default color modes ignore their payload; RGB and unknown modes
    // fall back to default attributes in this no-extras conversion. Raw
    // boundaries may split the scan, but must not split the final RLE.
    let aliases = [
        crate::PackedColors::DEFAULT,
        crate::PackedColors(0x0000_ffff),
        crate::PackedColors(0x0001_ffff),
        crate::PackedColors::DEFAULT.with_rgb_fg(),
        crate::PackedColors::DEFAULT.with_rgb_bg(),
        crate::PackedColors(0xff00_0000),
    ];
    for boundary in [1, 63, 64, 65] {
        let mut cells = Vec::new();
        for colors in aliases {
            cells.extend(std::iter::repeat_n(
                Cell::from_raw_parts(u16::from(b'x'), colors, CellFlags::BOLD),
                boundary,
            ));
        }
        check_both_paths(&cells, false);
        check_both_paths(&cells, true);
        assert_eq!(
            old_cell_loop(&cells, false).attrs().unwrap().runs().len(),
            1
        );
    }
}
