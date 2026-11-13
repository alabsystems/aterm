// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use crate::terminal::Terminal;
use aterm_types::CursorStyle;

fn decrqm(term: &mut Terminal, mode: u16) -> Vec<u8> {
    term.process(format!("\x1b[?{mode}$p").as_bytes());
    term.take_response().unwrap_or_default()
}

#[test]
fn mode_12_moves_the_cursor_the_user_sees_and_reports_it() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");

    term.process(b"\x1b[?12l\x1b[?25h");
    assert_eq!(
        term.cursor_style(),
        CursorStyle::SteadyBlock,
        "?12l must steady the cursor, not just set a flag"
    );
    assert!(term.cursor_visible());
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

    term.process(b"\x1b[6 q");
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");
    term.process(b"\x1b[?12;25h");
    assert_eq!(
        term.cursor_style(),
        CursorStyle::BlinkingBar,
        "?12h must blink the CURRENT shape"
    );
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");
}

#[test]
fn decscusr_moves_mode_12_with_the_style() {
    for (param, style, want_set) in [
        (1u16, CursorStyle::BlinkingBlock, true),
        (2, CursorStyle::SteadyBlock, false),
        (3, CursorStyle::BlinkingUnderline, true),
        (4, CursorStyle::SteadyUnderline, false),
        (5, CursorStyle::BlinkingBar, true),
        (6, CursorStyle::SteadyBar, false),
    ] {
        let mut term = Terminal::new(24, 80);
        term.process(format!("\x1b[{param} q").as_bytes());
        assert_eq!(term.cursor_style(), style);
        let want = if want_set {
            b"\x1b[?12;1$y".to_vec()
        } else {
            b"\x1b[?12;2$y".to_vec()
        };
        assert_eq!(decrqm(&mut term, 12), want, "DECSCUSR {param}");
        term.process(b"\x1bP$q?12\x1b\\");
        let decrqss = term.take_response().unwrap_or_default();
        let expect_decrqss: &[u8] = if want_set {
            b"\x1bP1$r?12h\x1b\\"
        } else {
            b"\x1bP1$r?12l\x1b\\"
        };
        assert_eq!(decrqss, expect_decrqss, "DECRQSS after DECSCUSR {param}");
    }
}

#[test]
fn mode_12_under_conhost_keeps_the_configured_cursor() {
    const VIM_START: &[u8] = b"\x1b[?1049h\x1b[?12h\x1b[?12l";
    const VIM_EXIT: &[u8] = b"\x1b[?25h\x1b[?1049l";

    let mut term = Terminal::new(24, 80);
    term.set_win32_input_enabled(true);
    term.process(b"\x1b[?9001h");
    assert!(
        term.modes().win32_input_mode,
        "the console-host mode is active"
    );
    term.process(VIM_START);
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;1$y");
    term.process(VIM_EXIT);
    assert_eq!(
        term.cursor_style(),
        CursorStyle::BlinkingBlock,
        "the prompt cursor still blinks after the alternate screen closes"
    );

    term.process(b"\x1b[?25l");
    assert!(!term.cursor_visible());
    term.process(b"\x1b[?12;25h");
    assert!(term.cursor_visible());
    assert_eq!(term.cursor_style(), CursorStyle::BlinkingBlock);

    term.process(b"\x1b[6 q");
    term.process(b"\x1b[?12h");
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBar);
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

    let mut term = Terminal::new(24, 80);
    term.set_default_cursor_style(CursorStyle::SteadyUnderline);
    term.set_win32_input_enabled(true);
    term.process(b"\x1b[?9001h\x1b[?12h");
    assert!(term.modes().win32_input_mode);
    assert_eq!(term.cursor_style(), CursorStyle::SteadyUnderline);
}

#[test]
fn mode_12_still_applies_when_unix_ignores_mode_9001() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?9001h");
    assert!(!term.modes().win32_input_mode);
    term.process(b"\x1b[?1049h\x1b[?12h\x1b[?12l");
    term.process(b"\x1b[?25h\x1b[?1049l");
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBlock);
    assert!(term.cursor_visible());
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");

    let mut term = Terminal::new(24, 80);
    term.set_win32_input_enabled(true);
    term.process(b"\x1b[?9001h");
    assert!(term.modes().win32_input_active());
    term.set_win32_input_enabled(false);
    assert!(term.modes().win32_input_mode, "the negotiated bit is stale");
    assert!(!term.modes().win32_input_active());
    term.process(b"\x1b[?12l");
    assert_eq!(term.cursor_style(), CursorStyle::SteadyBlock);
    assert_eq!(decrqm(&mut term, 12), b"\x1b[?12;2$y");
}

#[test]
fn xtsave_covers_every_mode_decrqm_reports() {
    let mut probe = Terminal::new(24, 80);
    probe.set_win32_input_enabled(true);
    let mut discovered = Vec::new();
    for mode in 0u16..=10_000 {
        let reply = decrqm(&mut probe, mode);
        if (reply.ends_with(b";1$y") || reply.ends_with(b";2$y")) && !matches!(mode, 2 | 1048) {
            discovered.push(mode);
        }
    }
    assert!(
        discovered.len() >= 38,
        "DECRQM discovery found unexpectedly few modes: {discovered:?}"
    );

    let mut lost = Vec::new();
    for mode in discovered {
        let mut term = Terminal::new(24, 80);
        if mode == 3 {
            term.process(b"\x1b[?40h");
        }
        if mode == 9001 {
            term.set_win32_input_enabled(true);
        }
        term.process(format!("\x1b[?{mode}h").as_bytes());
        let _ = term.take_response();
        let armed = decrqm(&mut term, mode);
        assert_eq!(
            armed,
            format!("\x1b[?{mode};1$y").into_bytes(),
            "mode {mode} did not arm"
        );
        term.process(format!("\x1b[?{mode}s").as_bytes());
        term.process(format!("\x1b[?{mode}l").as_bytes());
        let _ = term.take_response();
        assert_eq!(
            decrqm(&mut term, mode),
            format!("\x1b[?{mode};2$y").into_bytes(),
            "mode {mode} did not disarm"
        );
        term.process(format!("\x1b[?{mode}r").as_bytes());
        let _ = term.take_response();
        if decrqm(&mut term, mode) != format!("\x1b[?{mode};1$y").into_bytes() {
            lost.push(mode);
        }
    }
    assert!(
        lost.is_empty(),
        "XTSAVE/XTRESTORE silently dropped modes DECRQM reports: {lost:?}"
    );
}
