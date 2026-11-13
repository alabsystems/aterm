// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `set_kitty_keyboard_enabled(false)` must make the terminal behave as if
//! the kitty protocol is unsupported (the ConPTY embedder posture): no
//! `CSI ? u` reply, push/set/pop consumed-and-ignored, no kitty bits in the
//! derived keyboard mode, legacy key encoding throughout.
use super::shift_enter_e2e_tests::enc_enter;
use crate::terminal::Terminal;
use aterm_types::keyboard::{KeyboardMode, Modifiers};

const KITTY_BITS: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES
    .union(KeyboardMode::REPORT_EVENT_TYPES)
    .union(KeyboardMode::REPORT_ALTERNATE_KEYS)
    .union(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    .union(KeyboardMode::REPORT_ASSOCIATED_TEXT);

#[test]
fn disabled_query_gets_no_reply() {
    let mut term = Terminal::new(24, 80);
    term.set_kitty_keyboard_enabled(false);
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response(),
        None,
        "capability off: the progressive-enhancement query must get NO reply (not ?0u)"
    );
}

#[test]
fn disabled_negotiation_is_consumed_and_ignored() {
    let mut term = Terminal::new(24, 80);
    term.set_kitty_keyboard_enabled(false);
    term.process(b"\x1b[>1u");
    term.process(b"\x1b[=31;1u");
    term.process(b"\x1b[<u");
    assert_eq!(
        term.kitty_keyboard_flags().bits(),
        0,
        "push/set must not change stored flags while disabled"
    );
    assert!(
        !term.keyboard_mode().intersects(KITTY_BITS),
        "keyboard_mode must carry no kitty bits while disabled"
    );
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (0, 0), "sequence leaked to grid");
    assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
    assert_eq!(
        enc_enter(&term, Modifiers::SHIFT),
        vec![0x0a],
        "negotiation attempted while disabled must leave encoding legacy"
    );
    term.process(b"\x1b[?u");
    assert_eq!(term.take_response(), None);
}

#[test]
fn disabling_after_negotiation_drops_kitty_bits_and_reenabling_restores() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>1u");
    assert!(
        term.keyboard_mode()
            .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
    );
    term.set_kitty_keyboard_enabled(false);
    assert!(
        !term.keyboard_mode().intersects(KITTY_BITS),
        "already-negotiated flags must stop reaching keyboard_mode once disabled"
    );
    assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
    term.set_kitty_keyboard_enabled(true);
    assert_eq!(enc_enter(&term, Modifiers::SHIFT), b"\x1b[13;2u");
}

#[test]
fn disabled_survives_ris() {
    let mut term = Terminal::new(24, 80);
    term.set_kitty_keyboard_enabled(false);
    term.process(b"\x1bc");
    assert!(!term.is_kitty_keyboard_enabled());
    term.process(b"\x1b[?u");
    assert_eq!(term.take_response(), None);
}
