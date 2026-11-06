// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A cell's truecolour and its non-BMP character can live in the grid's dense rings, slots
//! keyed by position that belong to the cell only while its bits claim them (an RGB colour
//! mode, COMPLEX): a writer that overwrites the cell leaves the slots as they were, and every
//! reader asks the cell first. The column reflow of a resize copied the slots without asking,
//! so a cell the bulk ASCII writers had overwritten took the old glyph's colour into an extras
//! entry (and the HAS_EXTRAS bit) on the reflowed screen, where the per-character writer, which
//! keeps a glyph's colour in the extras map and removes it on overwrite, left the cell plain
//! (grid review 2026-10-30; the fuzz that met it masked the colour bits). Both paths now leave
//! the same cells, compared whole.

use std::sync::mpsc;
use std::time::Duration;

use aterm_core::grid::{Cell, CellExtra, LineSize};
use aterm_core::terminal::Terminal;
use aterm_spec::rng::XorShift;

/// A hang detector, not a latency budget: each input here is processed in microseconds.
const HANG: Duration = Duration::from_secs(60);

/// Designate ASCII as G1 and shift it in (SO): every glyph prints exactly as under G0, but the
/// bulk ASCII writers stand aside, so the run takes the per-character `write_char` path, the
/// reference.
const PER_CHARACTER: &[u8] = b"\x1b)B\x0e";

/// Output to process, or a resize of the screen.
enum Step {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

/// A truecolour pair as the cell's colour modes resolve it.
type Rgb = (Option<[u8; 3]>, Option<[u8; 3]>);

/// Per visible row: its line size, wrap flag, cells (all eight bytes), extras less their
/// truecolour, and the truecolour each cell's modes resolve; then the cursor and its pending
/// wrap. The writers keep a glyph's truecolour in different stores (a bulk run in the dense
/// ring, `write_char` in the extras map), so the colour is compared as the cell resolves it.
type Snapshot = (
    Vec<(LineSize, bool, Vec<Cell>, Vec<Option<CellExtra>>, Vec<Rgb>)>,
    (u16, u16, bool),
);

fn snapshot(t: &Terminal) -> Snapshot {
    let g = t.grid();
    let rows = (0..g.rows())
        .map(|r| {
            let row = g.row(r).expect("a visible row");
            let cells: Vec<Cell> = (0..g.cols())
                .map(|c| *g.cell(r, c).expect("a visible cell"))
                .collect();
            let extras = (0..g.cols())
                .map(|c| {
                    g.cell_extra(r, c).cloned().and_then(|mut extra| {
                        extra.set_fg_rgb(None);
                        extra.set_bg_rgb(None);
                        extra.has_data().then_some(extra)
                    })
                })
                .collect();
            let rgb = cells
                .iter()
                .zip(0..)
                .map(|(cell, c)| {
                    let colors = cell.colors();
                    (
                        colors.fg_is_rgb().then(|| g.fg_rgb_at(r, c)).flatten(),
                        colors.bg_is_rgb().then(|| g.bg_rgb_at(r, c)).flatten(),
                    )
                })
                .collect();
            (row.line_size(), row.is_wrapped(), cells, extras, rgb)
        })
        .collect();
    (rows, (g.cursor_row(), g.cursor_col(), g.pending_wrap()))
}

/// `steps` run on a fresh `rows`x`cols` terminal after `prefix`.
fn run(rows: u16, cols: u16, prefix: &[u8], steps: &[Step]) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    t.process(prefix);
    for step in steps {
        match step {
            Step::Bytes(bytes) => t.process(bytes),
            Step::Resize(rows, cols) => t.resize(*rows, *cols),
        }
    }
    t
}

/// The screen `steps` leave through the bulk writers and through `write_char`.
fn both_paths(rows: u16, cols: u16, steps: &[Step]) -> (Snapshot, Snapshot) {
    (
        snapshot(&run(rows, cols, b"", steps)),
        snapshot(&run(rows, cols, PER_CHARACTER, steps)),
    )
}

/// `f` on a thread watched by the hang detector.
fn watched<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(HANG)
        .expect("the inputs were processed (a minute without a return is a hang)")
}

#[test]
fn an_overwritten_truecolour_glyph_leaves_a_plain_cell_through_a_reflow() {
    // The review's input: a truecolour '3', DECRC (nothing saved: home, default rendition),
    // '}' over it, then a resize that reflows the columns. Then the same through the other
    // ways a pen leaves truecolour in the ring (a background, an erase's BCE fill) and the
    // other bulk writers that overwrite it (REP, an SGR run).
    let cases: [(&str, &[u8], char); 5] = [
        ("the review's input", b"\x1b[38;2;1;2;3m3\x1b8}", '}'),
        ("a background", b"\x1b[48;2;1;2;3m3\x1b8}", '}'),
        ("an erase's fill", b"\x1b[48;2;1;2;3m\x1b[K\x1b[0m}", '}'),
        ("REP over it", b"\x1b[38;2;1;2;3m3\x1b8\x1b[b", '3'),
        (
            "an SGR run over it",
            b"\x1b[38;2;1;2;3m33\x1b8\x1b[1m}}",
            '}',
        ),
    ];
    for (label, input, glyph) in cases {
        let steps = [Step::Bytes(input.to_vec()), Step::Resize(12, 28)];
        let ((bulk, reference), plain) = watched(move || {
            let t = run(13, 15, b"", &steps);
            let cell = *t.grid().cell(0, 0).expect("the home cell");
            (
                both_paths(13, 15, &steps),
                (cell, t.grid().cell_extra(0, 0).cloned()),
            )
        });
        assert_eq!(
            bulk, reference,
            "{label}: the bulk writers leave other cells than write_char after the reflow"
        );
        let (cell, extra) = plain;
        assert_eq!(cell.char(), glyph, "{label}");
        assert!(
            !cell.colors().fg_is_rgb() && !cell.colors().bg_is_rgb() && !cell.has_extras(),
            "{label}: the home cell claims no truecolour: {cell:?}"
        );
        assert_eq!(extra, None, "{label}: the home cell keeps no extras");
    }
}

#[test]
fn an_overwritten_non_bmp_glyph_leaves_a_plain_cell_through_a_reflow() {
    // The complex ring had the same reader: ASCII over a non-BMP glyph took the glyph back as
    // an extras entry on the reflowed screen, on both paths.
    for (label, input) in [
        ("narrow", "\u{1D400}\rA"),
        ("wide", "\u{1F600}\rA"),
        ("under REP", "x\u{1D400}y\r\x1b[3b"),
    ] {
        for (path, prefix) in [("bulk", &b""[..]), ("write_char", PER_CHARACTER)] {
            let steps = [Step::Bytes(input.as_bytes().to_vec()), Step::Resize(4, 7)];
            let row = watched(move || {
                let t = run(4, 10, prefix, &steps);
                let g = t.grid();
                (0..g.cols())
                    .map(|c| {
                        (
                            *g.cell(0, c).expect("a visible cell"),
                            g.cell_extra(0, c).cloned(),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            for (c, (cell, extra)) in row.into_iter().enumerate() {
                assert!(
                    !cell.is_complex() && !cell.has_extras(),
                    "{label} on {path}, column {c}: {cell:?}"
                );
                assert_eq!(extra, None, "{label} on {path}, column {c}: no extras");
            }
        }
    }
}

/// One generated run on a small screen: truecolour pens, the renditions and cursor restores
/// that drop them, the margin, line-size, erase and edit controls, text that reaches each bulk
/// writer, and resizes between them.
fn generated(rng: &mut XorShift) -> (u16, u16, Vec<Step>) {
    let mut below = |n: u16| u16::try_from(rng.below(u64::from(n))).expect("below a u16");
    let (rows, cols) = (1 + below(8), 1 + below(16));
    let mut steps = Vec::new();
    for _ in 0..4 + below(20) {
        let pick = |options: &[&'static [u8]], i: u16| options[usize::from(i) % options.len()];
        let i = below(12);
        let bytes: Vec<u8> = match below(24) {
            0..=2 => format!("\x1b[{};{}H", below(rows + 2), below(cols + 2)).into_bytes(),
            3 => pick(&[b"\x1b#6", b"\x1b#3", b"\x1b#5", b"\x1b#8"], i).to_vec(),
            4 => pick(&[b"\x1b[?69h", b"\x1b[?69l", b"\x1b[?6h", b"\x1b[?6l"], i).to_vec(),
            5 => format!("\x1b[{};{}s", below(cols + 2), below(cols + 2)).into_bytes(),
            6 => format!("\x1b[{};{}r", below(rows + 2), below(rows + 2)).into_bytes(),
            7 => pick(
                &[b"\r", b"\n", b"\x08", b"\r\n", b"\x1b[?7l", b"\x1b[?7h"],
                i,
            )
            .to_vec(),
            8 => format!("\x1b[{}b", 1 + below(2 * cols)).into_bytes(),
            9 | 10 => pick(&[b"\x1b[31m", b"\x1b[1m", b"\x1b[0m", b"\x1b[m"], i).to_vec(),
            11 => pick(&[b"\x1b]8;;http://x\x07", b"\x1b]8;;\x07"], i).to_vec(),
            12..=14 => format!(
                "\x1b[{};2;{};{};{}m",
                [38, 48][usize::from(i % 2)],
                below(256),
                below(256),
                below(256)
            )
            .into_bytes(),
            15 => pick(&[b"\x1b7", b"\x1b8", b"\x1b8", b"\x1b[s", b"\x1b[u"], i).to_vec(),
            16 => pick(
                &[b"\x1b[K", b"\x1b[1K", b"\x1b[2K", b"\x1b[J", b"\x1b[2J"],
                i,
            )
            .to_vec(),
            17 => {
                let op = ["@", "P", "X", "L", "M", "S", "T"][usize::from(i) % 7];
                format!("\x1b[{}{op}", 1 + below(4)).into_bytes()
            }
            18 | 19 => {
                steps.push(Step::Resize(1 + below(8), 1 + below(20)));
                continue;
            }
            _ => {
                let len = usize::from(1 + below(cols + 8));
                let mut text = Vec::new();
                while text.len() < len {
                    let byte = b'!' + u8::try_from(below(94)).expect("a printable byte");
                    let run = if below(4) == 0 { 1 + below(12) } else { 1 };
                    text.extend(std::iter::repeat_n(byte, usize::from(run)));
                }
                text
            }
        };
        steps.push(Step::Bytes(bytes));
    }
    (rows, cols, steps)
}

/// `steps` as text: output as its characters, a resize as `<rows>x<cols>`.
fn shown(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|step| match step {
            Step::Bytes(bytes) => format!("{:?}", String::from_utf8_lossy(bytes)),
            Step::Resize(rows, cols) => format!("<{rows}x{cols}>"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn generated_truecolour_and_resize_mixes_leave_the_same_cells_on_both_paths() {
    let diverged = watched(|| {
        let mut rng = XorShift::new(0x2647_B0C5);
        let mut diverged = Vec::new();
        for _ in 0..10_000 {
            let (rows, cols, steps) = generated(&mut rng);
            let (bulk, reference) = both_paths(rows, cols, &steps);
            if bulk != reference {
                diverged.push(format!("{rows}x{cols} {}", shown(&steps)));
            }
        }
        diverged
    });
    assert!(
        diverged.is_empty(),
        "{} generated runs leave other cells through the bulk writers than through \
         write_char, the first: {}",
        diverged.len(),
        diverged[0]
    );
}
