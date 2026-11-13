// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;
use aterm_types::keyboard::KeyboardMode;

#[test]
fn mode_67_toggles_backarrow_and_keyboard_mode() {
    let mut term = Terminal::new(24, 80);
    assert!(
        !term
            .keyboard_mode()
            .contains(KeyboardMode::BACKARROW_SENDS_BS)
    );
    term.process(b"\x1b[?67h");
    assert!(
        term.keyboard_mode()
            .contains(KeyboardMode::BACKARROW_SENDS_BS)
    );
    term.process(b"\x1b[?67l");
    assert!(
        !term
            .keyboard_mode()
            .contains(KeyboardMode::BACKARROW_SENDS_BS)
    );
}
