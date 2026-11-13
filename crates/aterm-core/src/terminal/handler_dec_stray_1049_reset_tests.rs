// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::grid::CellFlags;
use crate::terminal::Terminal;

fn cursor(term: &Terminal) -> (u16, u16) {
    let c = term.cursor();
    (c.row, c.col)
}

#[test]
fn a_stray_1049l_leaves_the_cursor_and_the_pen_put() {
    let mut term = Terminal::new(4, 20);
    term.process(b"\x1b[2;6H\x1b7\x1b[4;12H\x1b[1m\x1b[?1049l");
    assert!(!term.is_alternate_screen());
    assert_eq!(cursor(&term), (3, 11), "not back to the DECSC position");
    assert!(
        term.style().flags.contains(CellFlags::BOLD),
        "the pen stays"
    );
    let mut term = Terminal::new(4, 20);
    term.process(b"\t\x1b[1;31m\x1b[?1049l");
    assert_eq!(cursor(&term), (0, 8), "nor home with nothing saved");
    assert!(term.style().flags.contains(CellFlags::BOLD));
}

#[test]
fn a_1049l_from_the_alternate_screen_restores_the_slot_as_decrc_does() {
    type Between = fn(&mut Terminal);
    let cases: [(&str, &[u8], Between); 5] = [
        ("pen", b"\x1b[1;58;5;3m", |t| {
            t.process(b"\x1b[0;32;58;5;4m")
        }),
        ("G0", b"", |t| t.process(b"\x1b(0")),
        ("origin mode and clamp", b"\x1b[?6h", |t| {
            t.process(b"\x1b[2;3r\x1b[?6l");
        }),
        (
            "origin clamp to DECLRMM margins",
            b"\x1b[?6h\x1b[?69h",
            |t| {
                t.process(b"\x1b[3;4s");
            },
        ),
        ("wrap after a widen", b"ABCDE", |t| t.resize(3, 8)),
    ];
    for (label, before, between) in cases {
        let run = |save: &[u8], restore: &[u8]| {
            let mut term = Terminal::new(3, 5);
            term.process(before);
            term.process(save);
            between(&mut term);
            term.process(restore);
            term.process(b"q");
            let style = term.style();
            let rows: Vec<String> = (0..term.grid().rows())
                .map(|r| term.grid().row_text(r).unwrap_or_default())
                .collect();
            let grid = term.grid();
            let underline: Vec<(u16, u16, u32)> = (0..grid.rows())
                .flat_map(|r| (0..grid.cols()).map(move |c| (r, c)))
                .filter_map(|(r, c)| {
                    let color = grid.cell_extra(r, c)?.underline_color_u32()?;
                    Some((r, c, color))
                })
                .collect();
            (
                cursor(&term),
                term.modes().origin_mode,
                (style.fg, style.bg, style.flags),
                rows,
                underline,
            )
        };
        assert_eq!(
            run(b"\x1b[?1049h", b"\x1b[?1049l"),
            run(b"\x1b7", b"\x1b8"),
            "{label}"
        );
    }
}

#[test]
fn a_rmcup_after_the_orphaned_screen_recovery_keeps_the_cursor() {
    let mut term = Terminal::new(8, 30);
    term.process(b"out1\r\nout2\r\n$ vim\r\n\x1b[?1049h\x1b[H\x1b[2Jvim screen");
    term.process(b"\x1b]133;A\x07$ ");
    assert!(
        !term.is_alternate_screen(),
        "the prompt mark left the dead screen"
    );
    assert_eq!(
        cursor(&term),
        (3, 2),
        "the slot vim's ?1049h saved, plus the prompt"
    );
    term.process(b"ls\r\nfileA fileB\r\n\x1b]133;A\x07$ echo hi\r\nhi\r\n\x1b]133;A\x07$ ");
    term.process(b"tput rmcup\r\n\x1b[?1049l\x1b[23;0;0t\x1b]133;A\x07$ ");
    assert_eq!(cursor(&term), (7, 2), "the prompt lands below the output");
    let row = |r: u16| term.grid().row_text(r).unwrap_or_default();
    assert_eq!(
        row(3).trim_end(),
        "fileA fileB",
        "nothing overwrote the output"
    );
    assert_eq!(row(7).trim_end(), "$");
}

#[test]
fn a_47_screen_left_with_1049l_restores_like_xterm() {
    let mut term = Terminal::new(4, 20);
    term.process(b"\x1b[3;9H\x1b[?47h\x1b[2;2Hx\x1b[?1049l");
    assert!(!term.is_alternate_screen());
    assert_eq!(cursor(&term), (0, 0));
}

#[test]
fn a_restored_session_leaves_a_dead_1047_screen_where_the_live_one_would() {
    let mut live = Terminal::new(8, 30);
    live.process(b"out1\r\nout2\r\nout3\r\n$ app\r\n\x1b[?1047h\x1b[H\x1b[2Jfull screen\x1b[5;3H");
    let mut restored = Terminal::from_checkpoint(&live.checkpoint());
    for term in [&mut live, &mut restored] {
        assert!(term.is_alternate_screen());
        term.process(b"\x1b]133;A\x07$ ");
        assert!(
            !term.is_alternate_screen(),
            "the prompt mark left the dead screen"
        );
    }
    assert_eq!(cursor(&restored), cursor(&live));
    let row = |t: &Terminal, r: u16| t.grid().row_text(r).unwrap_or_default();
    assert_eq!(
        row(&restored, 0).trim_end(),
        "out1",
        "nothing overwrote the output"
    );
    for r in 0..8 {
        assert_eq!(row(&restored, r), row(&live, r), "row {r}");
    }
}
