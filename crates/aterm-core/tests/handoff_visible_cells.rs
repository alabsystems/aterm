// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A seamless update gives back the line sizes and the protected cells the live screen had.
//!
//! The visible screen crosses as `Line` records: text and attribute runs. A `Line` holds neither a
//! row's DEC line size nor a cell's DECSCA protection, so a DECDWL or DECDHL row came back single
//! width and a protected cell came back unprotected. Each test here carries a live engine through
//! the production pair (the capture `checkpoint_carry_abandoning_partial` and the adopt
//! `restore_checkpoint`, with the `serde` feature through `CheckpointMeta`, the seamless wire's
//! scalar half), holds the carried screen to the live one cell for cell, then drives both with
//! the same follow-up and holds them to each other again.

use aterm_core::grid::CellFlags;
use aterm_core::grid::row::LineSize;
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

const ROWS: u16 = 6;
const COLS: u16 = 40;

/// The production carry: the capture the seamless update takes, then the adopt into a fresh
/// engine.
fn carried(live: &Terminal) -> Terminal {
    let (checkpoint, abandoned) = live.checkpoint_carry_abandoning_partial(0);
    assert_eq!(
        abandoned, None,
        "the test streams leave no partial sequence"
    );
    let mut adopted = Terminal::new(live.rows(), live.cols());
    adopted.restore_checkpoint(&wire(checkpoint));
    adopted
}

/// The seamless wire's split: the scalar half through `CheckpointMeta`, the grid blobs beside it.
fn wire(checkpoint: TerminalCheckpoint) -> TerminalCheckpoint {
    #[cfg(feature = "serde")]
    let checkpoint = aterm_core::terminal::CheckpointMeta::from_checkpoint(&checkpoint)
        .into_checkpoint(checkpoint.grid.clone(), checkpoint.alt_grid.clone());
    checkpoint
}

/// One cell as a program and a selective erase see it: its text, whether it is a wide lead, and
/// bit 10 (a spacer's role or a cell's DECSCA protection; both sides of the comparison read it
/// the same way).
type CellView = (String, bool, bool);

/// The visible screen cell by cell, each row with its DEC line size.
fn screen(t: &Terminal) -> Vec<(LineSize, Vec<CellView>)> {
    let grid = t.grid();
    (0..grid.rows())
        .map(|r| {
            let row = grid.row(r).expect("visible row");
            let cells = (0..grid.cols())
                .map(|c| {
                    let flags = row.get(c).map_or(CellFlags::empty(), |cell| cell.flags());
                    (
                        t.cell_grapheme(usize::from(r), usize::from(c))
                            .unwrap_or_default(),
                        flags.contains(CellFlags::WIDE),
                        flags.contains(CellFlags::PROTECTED),
                    )
                })
                .collect();
            (row.line_size(), cells)
        })
        .collect()
}

fn rows_text(t: &Terminal) -> Vec<String> {
    (0..usize::from(t.rows()))
        .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect()
}

/// Carry `setup`, hold the carried screen to the live one, then drive both with `then` and hold
/// them to each other again (screen and the cursor position report).
fn assert_carried_exactly(label: &str, setup: &[u8], then: &[u8]) {
    let mut live = Terminal::new(ROWS, COLS);
    live.process(setup);
    let mut adopted = carried(&live);
    assert_eq!(
        screen(&adopted),
        screen(&live),
        "{label}: the carried screen holds the live cells"
    );
    let report = b"\x1b[6n";
    live.process(then);
    live.process(report);
    adopted.process(then);
    adopted.process(report);
    assert_eq!(
        rows_text(&adopted),
        rows_text(&live),
        "{label}: the program's next output lands where it does live"
    );
    assert_eq!(
        adopted.take_response(),
        live.take_response(),
        "{label}: the cursor reports the live position"
    );
    assert_eq!(
        screen(&adopted),
        screen(&live),
        "{label}: after the follow-up"
    );
}

/// The setup streams below leave what the tests are about on the live screen: without this, a
/// carry that dropped the facts would still equal a live screen that never had them.
#[test]
fn the_setups_put_line_sizes_and_protection_on_the_live_screen() {
    let mut live = Terminal::new(ROWS, COLS);
    live.process(b"\x1b#6WIDE\r\n\x1b#3TALL\r\n\x1b#4TALL\r\n\x1b[1\"qKEEP\x1b[0\"q gone");
    let live_screen = screen(&live);
    let sizes: Vec<LineSize> = live_screen.iter().map(|(size, _)| *size).collect();
    assert_eq!(
        sizes[..4],
        [
            LineSize::DoubleWidth,
            LineSize::DoubleHeightTop,
            LineSize::DoubleHeightBottom,
            LineSize::SingleWidth,
        ]
    );
    let protected: Vec<bool> = live_screen[3].1.iter().map(|cell| cell.2).collect();
    assert_eq!(protected[..6], [true, true, true, true, false, false]);
}

/// A DECDWL row and a DECDHL pair keep their line size, so a cursor move past the half and text
/// that wraps at the half land where they do live.
#[test]
fn a_double_width_or_double_height_row_keeps_its_line_size() {
    let setup = b"\x1b#6WIDE BANNER\r\n\x1b#3TALL\r\n\x1b#4TALL\r\nplain";
    for (label, then) in [
        (
            "a cursor move past the half",
            &b"\x1b[1;30HX\x1b[2;35HY"[..],
        ),
        (
            "text that wraps at the half",
            b"\x1b[1;1Hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        ),
        ("a tab past the half", b"\x1b[3;1H\t\t\tT"),
    ] {
        assert_carried_exactly(label, setup, then);
    }
    // Cells a later SR moved into the half the size hides stay there, as on the live row, so a
    // DECSWL shows them again: the restore sets the size without the blanking `ESC # 6` does.
    assert_carried_exactly(
        "cells an SR moved into the hidden half",
        b"\x1b#6abcdefghijklmnopqrs\x1b[2 A",
        b"\x1b#5",
    );
}

/// DECSCA-protected cells (a narrow run, one mid-row, a wide glyph) stay protected, so a
/// selective erase after the update keeps what it keeps live.
#[test]
fn a_protected_cell_stays_protected() {
    let setup = "\x1b[1\"qName:\x1b[0\"q Alice\r\n   \x1b[1\"qKEEP\x1b[0\"q gone\r\n\
                 \x1b[1\"q\u{4E16}\x1b[0\"q after";
    for (label, then) in [
        ("DECSED", "\x1b[?2J"),
        ("DECSEL", "\x1b[1;1H\x1b[?2K\x1b[2;1H\x1b[?2K"),
        ("DECSERA", "\x1b[1;1;6;40${"),
        // ED ignores DECSCA: the control that the erase above is selective.
        ("ED", "\x1b[2J"),
    ] {
        assert_carried_exactly(label, setup.as_bytes(), then.as_bytes());
    }
}

/// The inactive grid crosses with its own facts: a protected cell and a double-width row on the
/// saved primary while the alternate screen is up are kept, and read so once the program leaves
/// it.
#[test]
fn the_saved_primary_keeps_its_cells_while_the_alternate_screen_is_up() {
    let setup = "\x1b#6BANNER\r\n\x1b[1\"qMAIN\x1b[0\"q main\r\n\
                 \x1b[?1049h\x1b#6\x1b[1\"qALT\x1b[0\"q alt";
    assert_carried_exactly(
        "leaving the alternate screen",
        setup.as_bytes(),
        b"\x1b[?2J\x1b[1;30HA\x1b[?1049l\x1b[?2J\x1b[1;30HX",
    );
}
