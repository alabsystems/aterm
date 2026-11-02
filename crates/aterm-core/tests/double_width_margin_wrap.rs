// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An autowrap that lands on a double-width row (DECDWL) with the DECSLRM left margin past the
//! row's half width puts the cursor on a column the row has: every bulk ASCII writer then
//! finishes, and leaves the cells the per-character writer leaves. Before the fix the wrap put
//! the cursor on column `left` of a row only `cols / 2` wide, the bulk writers found no room
//! there and wrapped again, and at the bottom of the scroll region that never ended (37 bytes
//! of standard VT sequences spun `Terminal::process` forever).

use std::sync::mpsc;
use std::time::Duration;

use aterm_core::grid::{Cell, CellExtra, LineSize};
use aterm_core::terminal::Terminal;
use aterm_spec::rng::XorShift;

/// A hang detector, not a latency budget: each input here is processed in microseconds.
const HANG: Duration = Duration::from_secs(60);

/// Designate ASCII as G1 and shift it in (SO): every glyph prints exactly as under G0, but the
/// bulk ASCII writers stand aside (`print_ascii_bulk` and REP need the G0 pass-through), so the
/// run takes the per-character `write_char` path, the reference.
const PER_CHARACTER: &[u8] = b"\x1b)B\x0e";

/// `input` processed by a fresh `rows`x`cols` terminal, on a thread watched by the hang detector.
fn process(rows: u16, cols: u16, input: Vec<u8>) -> Terminal {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut t = Terminal::new(rows, cols);
        t.process(&input);
        let _ = tx.send(t);
    });
    rx.recv_timeout(HANG)
        .expect("Terminal::process returned (a minute without a return is a hang)")
}

/// Per visible row: its line size, wrap flag, cells and extras; then the cursor and its pending wrap.
type Snapshot = (
    Vec<(LineSize, bool, Vec<Cell>, Vec<Option<CellExtra>>)>,
    (u16, u16, bool),
);

fn snapshot(t: &Terminal) -> Snapshot {
    let g = t.grid();
    let rows = (0..g.rows())
        .map(|r| {
            let row = g.row(r).expect("a visible row");
            (
                row.line_size(),
                row.is_wrapped(),
                (0..g.cols())
                    .map(|c| *g.cell(r, c).expect("a visible cell"))
                    .collect(),
                (0..g.cols()).map(|c| g.cell_extra(r, c).cloned()).collect(),
            )
        })
        .collect();
    (rows, (g.cursor_row(), g.cursor_col(), g.pending_wrap()))
}

/// The two 37-byte inputs the input review saved, with the screen each hung.
const SAVED: [(u16, u16, &[u8]); 2] = [
    (
        24,
        80,
        b"\x1b[24;1H\x1b#6\x1b[?69h\x1b[46;80s\x1b[24;40Hab\x07cd",
    ),
    (
        10,
        20,
        b"\x1b[10;1H\x1b#6\x1b[?69h\x1b[11;20s\x1b[10;10Hab\x07cd",
    ),
];

/// The saved inputs' shape on `rows`x`cols`: DECDWL on the bottom row, DECLRMM, the left
/// margin at 1-based column `left` (at or past the row's half width), the cursor on the row's
/// last column left of the margin, then `tail`.
fn crasher(rows: u16, cols: u16, left: u16, tail: &[u8]) -> Vec<u8> {
    let mut v = format!(
        "\x1b[{rows};1H\x1b#6\x1b[?69h\x1b[{left};{cols}s\x1b[{rows};{half}H",
        half = cols / 2
    )
    .into_bytes();
    v.extend_from_slice(tail);
    v
}

#[test]
fn bulk_writes_past_a_double_width_row_finish_and_match_the_per_character_path() {
    // Each tail would reach one bulk writer from a cursor inside the margins: plain
    // (`write_ascii_blast`), REP (`write_cell_run`), SGR (`write_ascii_run_styled_packed`), an
    // open OSC 8 link (`write_ascii_run_with_extras`), and a run long enough to batch its
    // repeats (`write_cell_run` from `write_ascii_bulk_fast`). The wrap used to put the cursor
    // there, at the left margin, and the writer spun; it now lands on the row's last column,
    // left of the margin, where the tails take `write_char`. These cases hold the hang; the
    // next test sends the landing through the bulk writers.
    let long: Vec<u8> = b"ab\x07"
        .iter()
        .copied()
        .chain(std::iter::repeat_n(b'z', 40))
        .chain(b"xy".iter().copied().cycle().take(40))
        .collect();
    let tails: [(&str, &[u8]); 6] = [
        ("plain", b"ab\x07cd"),
        ("one byte", b"ab\x07c"),
        ("REP", b"ab\x07\x1b[10b"),
        ("SGR", b"ab\x07\x1b[31mcd"),
        ("OSC 8", b"ab\x07\x1b]8;;http://x\x07cd"),
        ("batched repeats", &long),
    ];
    let cases = SAVED
        .iter()
        .map(|&(rows, cols, input)| (format!("saved {rows}x{cols}"), rows, cols, input.to_vec()))
        .chain(
            [(24, 80, 46), (24, 80, 41), (10, 20, 11), (5, 9, 5)]
                .into_iter()
                .flat_map(|(rows, cols, left)| {
                    tails.iter().map(move |&(label, tail)| {
                        let at = format!("{rows}x{cols} left {left} {label}");
                        (at, rows, cols, crasher(rows, cols, left, tail))
                    })
                }),
        );
    for (at, rows, cols, input) in cases {
        let bulk = snapshot(&process(rows, cols, input.clone()));
        let reference = snapshot(&process(
            rows,
            cols,
            PER_CHARACTER.iter().chain(&input).copied().collect(),
        ));
        assert_eq!(
            bulk, reference,
            "{at}: the bulk writers leave other cells than write_char"
        );
        let (bottom, cursor) = (&bulk.0[usize::from(rows - 1)], bulk.1);
        assert_eq!(bottom.0, LineSize::DoubleWidth, "{at}");
        assert!(
            cursor.1 < cols / 2,
            "{at}: the cursor is on a column the row has: {cursor:?}"
        );
    }
}

/// A run printed inside the margins that wraps onto a double-width row whose half ends left of
/// the left margin, above the region bottom, goes on at that row's last column (where a line
/// feed onto the row would put the cursor), then at the left margin on the row after it. The
/// landing is made inside each bulk writer the tail reaches, and `write_char` makes the same.
/// Before the fix the writers skipped the double-width row and went on at the left margin of
/// the next one.
#[test]
fn a_run_wrapping_onto_a_double_width_row_left_of_the_margin_goes_on_at_its_last_column() {
    // 24x80, row 11 double-width (40 columns), margins 46..=80, the cursor at row 10 column 46:
    // 34 letters fill columns 46..=79, then three glyphs land at column 80, on row 11's last
    // column (40) and at the left margin of row 12.
    let fill: Vec<u8> = (0..34u8).map(|i| b'a' + i % 26).collect();
    let tails: [(&str, &[u8], [char; 3]); 4] = [
        ("plain", b"XYZ", ['X', 'Y', 'Z']),
        ("SGR", b"X\x1b[31mYZ", ['X', 'Y', 'Z']),
        ("OSC 8", b"X\x1b]8;;http://x\x07YZ", ['X', 'Y', 'Z']),
        ("REP", b"X\x1b[2b", ['X', 'X', 'X']),
    ];
    for (label, tail, glyphs) in tails {
        let mut input = b"\x1b[11;1H\x1b#6\x1b[?69h\x1b[46;80s\x1b[10;46H".to_vec();
        input.extend_from_slice(&fill);
        input.extend_from_slice(tail);
        let t = process(24, 80, input.clone());
        let bulk = snapshot(&t);
        let reference = snapshot(&process(
            24,
            80,
            PER_CHARACTER.iter().chain(&input).copied().collect(),
        ));
        assert_eq!(
            bulk, reference,
            "{label}: the bulk writers leave other cells than write_char"
        );
        let (rows, cursor) = &bulk;
        let text = |r: usize| t.row_text(r).unwrap_or_default();
        assert_eq!(rows[10].0, LineSize::DoubleWidth, "{label}");
        let landed = [(9, 79), (10, 39), (11, 45)].map(|(r, c)| rows[r].2[c].char());
        assert_eq!(landed, glyphs, "{label}: rows {:?}", [9, 10, 11].map(text));
        assert_eq!(
            text(10).trim(),
            glyphs[1].to_string(),
            "{label}: the double-width row holds only the wrapped glyph"
        );
        assert_eq!(*cursor, (11, 46, false), "{label}");
    }
}

/// One generated input on a small screen: a mix of the controls the hang needed (line sizes,
/// DECLRMM and DECSLRM, DECSTBM, DECOM, DECAWM, CUP, CR, LF, BS, BEL) and text that reaches
/// each bulk writer (plain, REP, SGR, OSC 8, runs long enough to batch their repeats).
fn generated(rng: &mut XorShift) -> (u16, u16, Vec<u8>) {
    let mut below = |n: u16| u16::try_from(rng.below(u64::from(n))).expect("below a u16");
    let (rows, cols) = (1 + below(10), 1 + below(24));
    let mut v = Vec::new();
    for _ in 0..4 + below(28) {
        let pick = |options: &[&'static [u8]], i: u16| options[usize::from(i) % options.len()];
        let i = below(12);
        match below(14) {
            0 | 1 => v.extend(format!("\x1b[{};{}H", below(rows + 2), below(cols + 2)).bytes()),
            2 => v.extend(pick(&[b"\x1b#6", b"\x1b#3", b"\x1b#4", b"\x1b#5"], i)),
            3 => v.extend(pick(&[b"\x1b[?69h", b"\x1b[?69l"], i)),
            4 => v.extend(format!("\x1b[{};{}s", below(cols + 2), below(cols + 2)).bytes()),
            5 => v.extend(format!("\x1b[{};{}r", below(rows + 2), below(rows + 2)).bytes()),
            6 => v.extend(pick(&[b"\x1b[?6h", b"\x1b[?6l"], i)),
            7 => v.extend(pick(&[b"\x1b[?7h", b"\x1b[?7h", b"\x1b[?7l"], i)),
            8 => v.extend(pick(&[b"\r", b"\n", b"\x08", b"\x07", b"\r\n"], i)),
            9 => v.extend(format!("\x1b[{}b", 1 + below(3 * cols)).bytes()),
            10 => v.extend(pick(&[b"\x1b[31m", b"\x1b[1m", b"\x1b[0m"], i)),
            11 => v.extend(pick(&[b"\x1b]8;;http://x\x07", b"\x1b]8;;\x07"], i)),
            _ => {
                let len = 1 + below(2 * cols + 70);
                let mut n = 0;
                while n < len {
                    let byte = b'a' + u8::try_from(below(26)).expect("a letter");
                    let run = if below(4) == 0 { 1 + below(12) } else { 1 };
                    v.extend(std::iter::repeat_n(byte, usize::from(run)));
                    n += run;
                }
            }
        }
    }
    (rows, cols, v)
}

#[test]
fn generated_line_size_and_margin_mixes_finish_and_match_the_per_character_path() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut rng = XorShift::new(0xD3CD_31A7);
        let mut diverged = Vec::new();
        for _ in 0..3000 {
            let (rows, cols, input) = generated(&mut rng);
            let run = |bytes: &[u8]| {
                let mut t = Terminal::new(rows, cols);
                t.process(bytes);
                snapshot(&t)
            };
            let reference: Vec<u8> = PER_CHARACTER.iter().chain(&input).copied().collect();
            if run(&input) != run(&reference) {
                diverged.push(format!(
                    "{rows}x{cols} {:?}",
                    String::from_utf8_lossy(&input)
                ));
            }
        }
        let _ = tx.send(diverged);
    });
    let diverged = rx
        .recv_timeout(HANG)
        .expect("every generated input was processed (a minute without a return is a hang)");
    assert!(
        diverged.is_empty(),
        "{} generated inputs leave other cells through the bulk writers than through \
         write_char, the first: {}",
        diverged.len(),
        diverged[0]
    );
}
