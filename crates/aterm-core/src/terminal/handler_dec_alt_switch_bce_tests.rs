// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;

fn bg_index(term: &Terminal, row: u16, col: u16) -> Option<u8> {
    let colors = term.grid().cell(row, col)?.colors();
    colors.bg_is_indexed().then(|| colors.bg_index())
}

#[test]
fn a_line_scrolled_in_on_the_switched_in_buffer_takes_the_current_background() {
    for enter in [&b"\x1b[?47h"[..], b"\x1b[?1047h"] {
        let mut term = Terminal::new(3, 10);
        term.process(b"\x1b[44m");
        term.process(enter);
        term.process(b"\x1b[3;1H\n");
        assert_eq!(bg_index(&term, 2, 0), Some(4), "entering with {enter:?}");
    }
    let mut term = Terminal::new(3, 10);
    term.process(b"\x1b[?47h\x1b[42m\x1b[?47l\x1b[3;1H\n");
    assert_eq!(bg_index(&term, 2, 0), Some(2), "leaving ?47");

    let mut live = Terminal::new(3, 10);
    live.process(b"\x1b[44m\x1b[?1049h\x1b[0m");
    let mut restored = Terminal::from_checkpoint(&live.checkpoint());
    for term in [&mut live, &mut restored] {
        term.process(b"\x1b[?1049l\x1b[3;1H\n");
    }
    assert_eq!(bg_index(&live, 2, 0), Some(4), "leaving ?1049");
    assert_eq!(
        bg_index(&restored, 2, 0),
        Some(4),
        "leaving ?1049, restored"
    );
}

#[test]
fn an_open_link_ends_at_every_screen_switch() {
    for (on, switch) in [
        ("", "?1049h"),
        ("?1049h", "?1049l"),
        ("", "?1047h"),
        ("?1047h", "?1047l"),
    ] {
        let mut term = Terminal::new(3, 10);
        if !on.is_empty() {
            term.process(format!("\x1b[{on}").as_bytes());
        }
        term.process(b"\x1b]8;;https://example.com/\x07");
        term.process(format!("\x1b[{switch}").as_bytes());
        let at = term.cursor();
        term.process(b"x");
        assert_eq!(term.hyperlink_at(at.row, at.col), None, "{switch}");
    }
}
