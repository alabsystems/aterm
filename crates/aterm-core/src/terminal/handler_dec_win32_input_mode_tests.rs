// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;
use aterm_types::keyboard::KeyboardMode;

fn conpty_term() -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.set_win32_input_enabled(true);
    term
}

#[test]
fn ris_clears_mode_9001() {
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    term.process(b"\x1bc");
    assert!(!term.modes().win32_input_mode);
    assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
}

#[test]
fn mode_9001_survives_decstr() {
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    term.process(b"\x1b[!p");
    assert!(term.modes().win32_input_mode);
}
