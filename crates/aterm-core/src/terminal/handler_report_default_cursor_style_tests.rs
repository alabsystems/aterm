// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::{CursorStyle, Terminal};

#[test]
fn resets_return_alternate_scroll_to_the_hosts() {
    for reset in [
        &b"\x1bc"[..],
        b"\x1b[!p",
        b"\x1bc\x1b[!p\x1b[?3;4l\x1b[4l\x1b>",
    ] {
        let mut term = Terminal::new(24, 80);
        term.set_host_alternate_scroll(true);
        term.process(b"\x1b[?1007l");
        term.process(reset);
        assert!(term.modes().alternate_scroll, "{reset:?}");
        assert!(
            term.mode_mirror().alternate_scroll(),
            "{reset:?}: the wheel's word"
        );
    }
    let mut term = Terminal::new(24, 80);
    term.set_host_alternate_scroll(true);
    term.reset();
    assert!(term.modes().alternate_scroll, "Terminal::reset");
}

#[test]
fn host_default_takes_effect_before_app_decscusr() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock); // engine default
    term.set_default_cursor_style(CursorStyle::SteadyBar);
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
}

#[test]
fn host_default_does_not_clobber_app_decscusr() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[3 q");
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingUnderline);
    term.set_default_cursor_style(CursorStyle::SteadyBar);
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingUnderline);
}

#[test]
fn full_reset_restores_host_default_not_blinking_block() {
    let mut term = Terminal::new(24, 80);
    term.set_default_cursor_style(CursorStyle::SteadyUnderline);
    term.process(b"\x1b[5 q"); // app override to blinking bar
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingBar);
    term.process(b"\x1bc"); // RIS full reset
    assert_eq!(term.cursor_style(), CursorStyle::SteadyUnderline);
}

#[test]
fn apply_config_cursor_style_persists_as_default() {
    use crate::config::TerminalConfig;
    let mut term = Terminal::new(24, 80);
    let config = TerminalConfig {
        cursor_style: CursorStyle::SteadyBar,
        ..TerminalConfig::default()
    };
    term.apply_config(&config);
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
    term.process(b"\x1b[3 q");
    term.process(b"\x1b[!p");
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
}
