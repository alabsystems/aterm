// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic panic-freedom and invariant fuzz tests for the VT terminal engine.

use aterm_core::terminal::Terminal;

#[inline]
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

fn check_invariants(term: &Terminal) {
    let (rows, cols) = (term.rows(), term.cols());
    assert!(
        rows > 0 && cols > 0,
        "dimensions must stay positive, got {rows}x{cols}"
    );
    let cur = term.cursor();
    assert!(
        cur.row < rows,
        "cursor row {} escaped bounds (rows {rows})",
        cur.row
    );
    assert!(
        cur.col <= cols,
        "cursor col {} escaped bounds (cols {cols})",
        cur.col
    );
    term.grid().assert_structural_invariants();
    term.grid().assert_wide_char_consistent();
}

fn check_extras_claimed(term: &Terminal) {
    term.grid().assert_extras_claimed();
}

fn walk_all_cells(term: &Terminal) {
    let (rows, cols) = (term.rows(), term.cols());
    for r in 0..rows {
        let Some(row) = term.grid().row(r) else {
            continue;
        };
        for c in 0..cols {
            if let Some(cell) = row.get(c) {
                let _ = cell.char();
            }
        }
    }
}

#[test]
fn process_arbitrary_bytes_never_panics() {
    let mut s = 0xC0FF_EE12_3456_789Au64;
    let mut term = Terminal::new(24, 80);
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    for _ in 0..100_000u32 {
        buf.clear();
        let len = (next(&mut s) % 48) as usize;
        for _ in 0..len {
            buf.push((next(&mut s) & 0xFF) as u8);
        }
        term.process(&buf);
        check_invariants(&term);
        check_extras_claimed(&term);
    }
    walk_all_cells(&term);
}

#[test]
fn process_crafted_escape_sequences_never_panics() {
    let mut s = 0x0BAD_C0DE_99C0_1A57u64;
    let mut term = Terminal::new(24, 80);
    term.set_scrollback_line_limit(Some(RESIZE_FUZZ_SCROLLBACK_LINES));
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    for _ in 0..100_000u32 {
        buf.clear();
        let seqs = 1 + next(&mut s) % 5;
        for _ in 0..seqs {
            emit_escape(&mut s, &mut buf);
        }
        term.process(&buf);
        check_invariants(&term);
        check_extras_claimed(&term);
        if next(&mut s).is_multiple_of(64) {
            let r = (1 + next(&mut s) % 60) as u16;
            let c = (1 + next(&mut s) % 160) as u16;
            term.resize(r, c);
            check_invariants(&term);
            check_extras_claimed(&term);
        }
    }
    walk_all_cells(&term);
}

#[test]
fn process_unicode_edges_never_panics() {
    let mut s = 0xFEED_FACE_7717_2390u64;
    let mut term = Terminal::new(10, 12);
    let samples: &[&[u8]] = &[
        b"\xe6\x97\xa5",
        b"\xe6\x9c\xac", // CJK 日本
        b"\xf0\x9f\x9a\x80",
        b"\xf0\x9f\x91\x8d",    // 🚀 👍
        "\u{1f3fd}".as_bytes(), // skin-tone modifier
        "\u{200d}".as_bytes(),  // ZWJ
        "\u{1f1fa}".as_bytes(), // regional indicator U
        "\u{1f1f8}".as_bytes(), // regional indicator S
        "\u{0301}".as_bytes(),  // combining acute
        "\u{fe0f}".as_bytes(),  // VS16
        "\u{fe0e}".as_bytes(),  // VS15
        b"\xf0",                // truncated 4-byte lead
        b"\xe6\x97",            // truncated 3-byte
        b"\x80",                // lone continuation
        b"\xff",                // invalid
    ];
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    for _ in 0..100_000u32 {
        buf.clear();
        let n = 1 + next(&mut s) % 8;
        for _ in 0..n {
            let pick = (next(&mut s) as usize) % samples.len();
            buf.extend_from_slice(samples[pick]);
            if next(&mut s).is_multiple_of(5) {
                buf.push(b'\n');
            }
            if next(&mut s).is_multiple_of(7) {
                buf.push(b'\r');
            }
        }
        term.process(&buf);
        check_invariants(&term);
        check_extras_claimed(&term);
    }
    walk_all_cells(&term);
}

fn check_invariants_reflow(term: &Terminal) {
    check_invariants(term);
    let (rows, cols) = (term.rows(), term.cols());
    let total = term.grid().total_lines();
    assert!(
        total >= rows as usize,
        "total_lines {total} < visible rows {rows} ({rows}x{cols})"
    );
    assert_eq!(
        term.grid().display_offset(),
        0,
        "the cell oracle (walk_visible_cells_strict) is ring-only and therefore offset-0-only"
    );
}

fn walk_visible_cells_strict(term: &Terminal) {
    check_invariants_reflow(term);
    let (rows, cols) = (term.rows(), term.cols());
    for r in 0..rows {
        let row = term.grid().row(r);
        for c in 0..cols {
            let Some(cell) = row.and_then(|row| row.get(c)) else {
                panic!("cell ({r},{c}) inaccessible on a {rows}x{cols} grid");
            };
            let _ = cell.char();
        }
    }
}

fn probe_lines(term: &Terminal) {
    let rows = i32::from(term.rows());
    for r in 0..rows {
        let _ = term.get_line_text(r, None);
    }
    let depth = term.grid().history_line_count();
    let recent = 0..depth.min(16);
    let strided = (0..depth).step_by((depth / 16).max(1));
    let edges = [depth.saturating_sub(1), depth];
    for rev in recent.chain(strided).chain(edges) {
        let rev = i32::try_from(rev).expect("history depth fits i32");
        let _ = term.get_line_text(-1 - rev, None);
    }
}

const RESIZE_FUZZ_SCROLLBACK_LINES: usize = 250;

fn emit_reflow_content(s: &mut u64, buf: &mut Vec<u8>) {
    const WIDE: &[&[u8]] = &[
        "日".as_bytes(),
        "本".as_bytes(),
        "語".as_bytes(),
        "界".as_bytes(),
        "🚀".as_bytes(),
        "👍".as_bytes(),
    ];
    match next(s) % 6 {
        0 => buf.extend_from_slice(WIDE[(next(s) as usize) % WIDE.len()]),
        1 => {
            let n = 1 + next(s) % 12;
            for _ in 0..n {
                buf.push((0x41 + (next(s) % 26)) as u8);
            }
        }
        2 => buf.extend_from_slice(b"\r\n"),
        3 => {
            buf.extend_from_slice(b"\x1b[38;5;");
            for d in (next(s) % 256).to_string().bytes() {
                buf.push(d);
            }
            buf.push(b'm');
        }
        4 => buf.push(b'\n'),
        _ => {
            buf.push((0x61 + (next(s) % 26)) as u8);
            buf.extend_from_slice("\u{0301}".as_bytes());
        }
    }
}

fn next_dim_pair(s: &mut u64) -> (u16, u16) {
    match next(s) % 8 {
        0 => (1, 1),
        1 => (1, 1 + (next(s) % 200) as u16),
        2 => (1 + (next(s) % 80) as u16, 1),
        3 => (1 + (next(s) % 4) as u16, 1 + (next(s) % 8) as u16),
        _ => (1 + (next(s) % 60) as u16, 1 + (next(s) % 200) as u16),
    }
}

fn screen_text(term: &Terminal) -> Vec<String> {
    (0..usize::from(term.rows()))
        .map(|r| term.row_text(r).unwrap_or_default())
        .collect()
}

fn process_checking_bottom_line_feeds(term: &mut Terminal, buf: &[u8]) {
    let mut rest = buf;
    while let Some(lf) = rest.iter().position(|&b| b == b'\n') {
        if lf > 0 {
            term.process(&rest[..lf]);
        }
        rest = &rest[lf + 1..];
        let rows = term.rows();
        let region = term.grid().scroll_region();
        let bottom_of_full_screen = term.cursor().row + 1 == rows
            && region.top == 0
            && region.bottom + 1 == rows
            && !term.grid().has_horizontal_margins()
            && term.grid().display_offset() == 0;
        if !bottom_of_full_screen {
            term.process(b"\n");
            continue;
        }
        let before = screen_text(term);
        term.process(b"\n");
        let after = screen_text(term);
        let n = before.len();
        if let Some(r) = (0..n - 1).find(|&r| after[r] != before[r + 1]) {
            panic!(
                "a line feed at the bottom of the {rows}x{} screen left row {r} = {:?}, \
                 want the row below it, {:?}",
                term.cols(),
                after[r],
                before[r + 1]
            );
        }
        assert!(
            after[n - 1].trim().is_empty(),
            "a line feed at the bottom of the {rows}x{} screen left a non-blank bottom row {:?}",
            term.cols(),
            after[n - 1]
        );
    }
    if !rest.is_empty() {
        term.process(rest);
    }
}

#[test]
fn reflow_wide_char_resize_never_panics() {
    let mut s = 0x5EED_1234_ABCD_9001u64;
    let mut term = Terminal::new(24, 80);
    term.set_scrollback_line_limit(Some(RESIZE_FUZZ_SCROLLBACK_LINES));
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    for _ in 0..30_000u32 {
        buf.clear();
        let chunks = 1 + next(&mut s) % 10;
        for _ in 0..chunks {
            emit_reflow_content(&mut s, &mut buf);
        }
        process_checking_bottom_line_feeds(&mut term, &buf);
        walk_visible_cells_strict(&term);
        check_extras_claimed(&term);
        if next(&mut s).is_multiple_of(3) {
            let (r, c) = next_dim_pair(&mut s);
            term.resize(r, c);
            walk_visible_cells_strict(&term);
            check_extras_claimed(&term);
            probe_lines(&term);
        }
    }
    walk_visible_cells_strict(&term);
}

#[test]
fn a_decdwl_row_resized_through_its_wide_pair_stays_consistent() {
    let mut term = Terminal::new(4, 112);
    let mut bytes = b"\x1b#6".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', 45));
    bytes.extend("\u{A0E2}".as_bytes());
    term.process(&bytes);
    term.grid().assert_wide_char_consistent();
    assert!(
        term.grid().is_wide_continuation_at(0, 46),
        "the pair is intact before the resize"
    );
    term.resize(4, 46);
    term.grid().assert_wide_char_consistent();
}

#[test]
fn an_alt_screen_width_cut_through_a_wide_pair_drops_its_extras() {
    for glyph in ["\u{1F680}", "\u{4E2D}\u{301}"] {
        let mut term = Terminal::new(2, 4);
        term.process(
            format!("\x1b[?1049h\x1b]8;;https://example.com/\x1b\\{glyph}\x1b]8;;\x1b\\")
                .as_bytes(),
        );
        assert!(
            term.hyperlink_at(0, 0).is_some(),
            "{glyph:?}: linked before"
        );
        term.resize(2, 3);
        term.resize(2, 1);
        assert!(
            term.grid().cell(0, 0).is_some_and(|c| !c.has_extras()),
            "{glyph:?}: the cut blanks the head"
        );
        assert_eq!(
            term.hyperlink_at(0, 0),
            None,
            "{glyph:?}: the blank still links the cut glyph's target"
        );
        check_extras_claimed(&term);
    }
}

#[test]
fn alt_screen_resize_never_panics() {
    let mut s = 0xA17E_5C9E_2026_0601u64;
    let mut term = Terminal::new(24, 80);
    term.set_scrollback_line_limit(Some(RESIZE_FUZZ_SCROLLBACK_LINES));
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut in_alt = false;
    for _ in 0..40_000u32 {
        buf.clear();
        if next(&mut s).is_multiple_of(8) {
            buf.extend_from_slice(if in_alt {
                b"\x1b[?1049l"
            } else {
                b"\x1b[?1049h"
            });
            in_alt = !in_alt;
        }
        let chunks = 1 + next(&mut s) % 8;
        for _ in 0..chunks {
            emit_reflow_content(&mut s, &mut buf);
        }
        process_checking_bottom_line_feeds(&mut term, &buf);
        check_invariants(&term);
        check_extras_claimed(&term);
        if next(&mut s).is_multiple_of(3) {
            let (r, c) = next_dim_pair(&mut s);
            term.resize(r, c);
            check_invariants(&term);
            check_extras_claimed(&term);
            probe_lines(&term);
        }
    }
    walk_all_cells(&term);
}

fn emit_escape(s: &mut u64, buf: &mut Vec<u8>) {
    buf.push(0x1b); // ESC
    match next(s) % 9 {
        0 => {
            buf.push(b'[');
            if next(s).is_multiple_of(4) {
                buf.push(b'?');
            }
            let np = next(s) % 6;
            for _ in 0..np {
                for d in next(s).to_string().bytes() {
                    buf.push(d);
                }
                buf.push(if next(s).is_multiple_of(4) {
                    b':'
                } else {
                    b';'
                });
            }
            let ni = next(s) % 3;
            for _ in 0..ni {
                buf.push((0x20 + (next(s) % 0x10)) as u8);
            }
            buf.push((0x40 + (next(s) % 0x3f)) as u8);
        }
        1 => {
            buf.push(b']');
            for d in (next(s) % 1000).to_string().bytes() {
                buf.push(d);
            }
            buf.push(b';');
            let bl = next(s) % 40;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            match next(s) % 3 {
                0 => buf.push(0x07),
                1 => buf.extend_from_slice(b"\x1b\\"),
                _ => {}
            }
        }
        2 => {
            buf.push(b'P');
            let bl = next(s) % 48;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            buf.extend_from_slice(b"\x1b\\");
        }
        3 => {
            buf.push(b'#');
            buf.push((0x30 + (next(s) % 0x10)) as u8);
        }
        4 => {
            buf.push((0x28 + (next(s) % 4)) as u8);
            buf.push((next(s) & 0xFF) as u8);
        }
        5 => {
            buf.push(b'_');
            let bl = next(s) % 24;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            buf.extend_from_slice(b"\x1b\\");
        }
        6 => {
            buf.push(b'^');
            let bl = next(s) % 24;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            buf.extend_from_slice(b"\x1b\\");
        }
        7 => {
            buf.push((0x40 + (next(s) % 0x20)) as u8);
        }
        _ => {
            let bl = next(s) % 24;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
        }
    }
}

mod resize_reflow {
    use aterm_core::terminal::Terminal;

    #[inline]
    fn next(s: &mut u64) -> u32 {
        *s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*s >> 33) as u32
    }

    fn emit_content(s: &mut u64, term: &mut Terminal) {
        const SAMPLES: &[&[u8]] = &[
            b"the quick brown fox jumps over the lazy dog 0123456789 ", // long -> wraps
            "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}".as_bytes(), // wide CJK
            "\u{1f680}\u{1f600}\u{2764}\u{fe0f}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}"
                .as_bytes(), // emoji + ZWJ
            "e\u{0301}n\u{0303}o\u{0308}a\u{030a}".as_bytes(), // combining marks
            b"\tTABBED\t",
            b"\x1b[31mred\x1b[1mbold\x1b[0m ",
            b"\r\n",
            b"\x1b[H", // home
            b"X",      // single cell at the current position (exercises pending-wrap)
        ];
        let n = 1 + next(s) % 10;
        for _ in 0..n {
            term.process(SAMPLES[(next(s) as usize) % SAMPLES.len()]);
        }
    }

    fn check_invariants(term: &Terminal, it: u32, r: u16, c: u16) {
        assert_eq!(
            term.rows(),
            r,
            "iter {it}: rows {} != resize target {r}",
            term.rows()
        );
        assert_eq!(
            term.cols(),
            c,
            "iter {it}: cols {} != resize target {c}",
            term.cols()
        );
        let cur = term.cursor();
        assert!(
            cur.row < r,
            "iter {it}: cursor row {} out of bounds (rows {r})",
            cur.row
        );
        assert!(
            cur.col <= c,
            "iter {it}: cursor col {} out of bounds (cols {c})",
            cur.col
        );
    }

    #[test]
    fn resize_reflow_keeps_engine_invariants_under_the_conpty_policy() {
        use aterm_core::grid::ResizePolicy;
        let mut s: u64 = 0xD1CE_F00D_1234_5678;
        let mut term = Terminal::new(24, 80);
        term.set_scrollback_line_limit(Some(super::RESIZE_FUZZ_SCROLLBACK_LINES));
        for it in 0..6_000u32 {
            emit_content(&mut s, &mut term);
            if next(&mut s) & 1 == 0 {
                let r = 1 + (next(&mut s) % 50) as u16;
                let c = if next(&mut s).is_multiple_of(3) {
                    term.cols()
                } else {
                    1 + (next(&mut s) % 200) as u16
                };
                let rows_only_grow = c == term.cols() && r > term.rows();
                let history = term.grid().scrollback_lines();
                let keys = (
                    term.grid().oldest_absolute_row(),
                    term.grid().visible_to_absolute(0),
                    term.grid().history_renumber_epoch(),
                );
                term.resize_with_policy(r, c, ResizePolicy::ConPty);
                check_invariants(&term, it, r, c);
                if rows_only_grow {
                    assert_eq!(
                        term.grid().scrollback_lines(),
                        history,
                        "iter {it}: a ConPTY rows-only grow revealed history"
                    );
                    assert_eq!(
                        (
                            term.grid().oldest_absolute_row(),
                            term.grid().visible_to_absolute(0),
                            term.grid().history_renumber_epoch(),
                        ),
                        keys,
                        "iter {it}: a ConPTY rows-only grow moved an absolute key"
                    );
                }
            }
        }
        let (rows, cols) = (term.rows(), term.cols());
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = term.grid().cell(row, col) {
                    let _ = cell.char();
                }
            }
        }
    }
}

mod fresh;
