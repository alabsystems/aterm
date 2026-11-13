// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;

#[test]
fn a_cursor_only_change_damages_the_cursor_cell_not_the_whole_grid() {
    for (name, setup, seq) in [
        ("DECTCEM hide", &b""[..], &b"\x1b[?25l"[..]),
        ("DECTCEM show", &b"\x1b[?25l"[..], &b"\x1b[?25h"[..]),
        ("DECSCUSR shape", &b"\x1b[2 q"[..], &b"\x1b[6 q"[..]),
        ("mode 12 blink", &b"\x1b[?12l"[..], &b"\x1b[?12h"[..]),
    ] {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[9;20H");
        term.process(setup);
        term.take_damage();
        assert!(!term.has_damage(), "{name}: the fixture starts clean");

        term.process(seq);

        assert!(
            term.has_damage(),
            "{name}: a cursor-only change must still damage, or the \
             frontend's redraw early-out swallows it"
        );
        assert!(
            !term.grid().damage().is_full(),
            "{name}: a cursor-only change must NOT damage the whole grid"
        );
        assert!(
            term.grid().damage().is_row_damaged(8),
            "{name}: the cursor's own row (0-based 8) carries the damage"
        );
        assert!(
            !term.grid().damage().is_row_damaged(0),
            "{name}: an unrelated row is untouched"
        );
    }
}

#[test]
fn leaving_the_alternate_screen_marks_the_restored_grid_fully_damaged() {
    for (enter, exit) in [
        (&b"\x1b[?1049h"[..], &b"\x1b[?1049l"[..]),
        (b"\x1b[?1047h", b"\x1b[?1047l"),
        (b"\x1b[?47h", b"\x1b[?47l"),
    ] {
        let name = String::from_utf8_lossy(exit);
        let mut term = Terminal::new(8, 40);
        term.process(b"main text");
        term.take_damage();
        term.process(enter);
        term.process(b"alt text");
        let _ = term.damage_epoch();
        term.take_damage();
        let before = term.damage_epoch();
        term.process(exit);
        assert!(
            term.grid().damage().is_full(),
            "{name}: the restored main grid must be fully damaged"
        );
        assert!(
            term.damage_epoch() > before,
            "{name}: the swap back must advance the damage epoch"
        );
    }
}

#[test]
fn a_whole_screen_change_still_marks_the_whole_grid() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[9;20H");
    term.take_damage();
    term.process(b"\x1b[?5h");
    assert!(
        term.grid().damage().is_full(),
        "reverse video repaints every cell and must mark the grid full"
    );
}
