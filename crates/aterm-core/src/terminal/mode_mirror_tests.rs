// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Every way the fold's inputs move, through `process()` and every host mutator, leaves the
//! mirror equal to the fold.
use super::super::Terminal;
use aterm_types::keyboard::KeyboardMode;
use aterm_types::mouse::MouseMode;

fn assert_in_sync(term: &Terminal, what: &str) {
    let m = term.mode_mirror();
    assert_eq!(
        m.keyboard_mode(),
        term.keyboard_mode(),
        "keyboard after {what}"
    );
    assert_eq!(m.mouse_mode(), term.mouse_mode(), "mouse after {what}");
    assert_eq!(
        m.mouse_tracking_enabled(),
        term.mouse_tracking_enabled(),
        "tracking after {what}"
    );
    assert_eq!(
        m.focus_reporting_enabled(),
        term.focus_reporting_enabled(),
        "focus reporting after {what}"
    );
    assert_eq!(m.focus_arms(), term.focus_arms(), "focus arms after {what}");
    assert_eq!(
        m.alternate_screen(),
        term.is_alternate_screen(),
        "alternate screen after {what}"
    );
    assert_eq!(
        m.alternate_scroll(),
        term.modes().alternate_scroll,
        "alternate scroll after {what}"
    );
    assert_eq!(
        m.bracketed_paste(),
        term.modes().bracketed_paste,
        "bracketed paste after {what}"
    );
}

#[test]
fn a_fresh_terminal_publishes_its_fold_not_an_empty_word() {
    let term = Terminal::new(24, 80);
    assert_in_sync(&term, "construction");
}

#[test]
fn every_process_driven_mutation_refreshes_the_mirror() {
    let mut term = Terminal::new(24, 80);
    let steps: &[(&str, &[u8])] = &[
        ("kitty push disambiguate", b"\x1b[>1u"),
        ("kitty set report-all", b"\x1b[=8;1u"),
        ("kitty pop", b"\x1b[<u"),
        ("DECCKM set", b"\x1b[?1h"),
        ("DECKPAM", b"\x1b="),
        ("DECBKM set", b"\x1b[?67h"),
        ("altSendsEscape reset", b"\x1b[?1039l"),
        ("metaSendsEscape set", b"\x1b[?1036h"),
        ("numLock reset", b"\x1b[?1035l"),
        ("modifyOtherKeys 2", b"\x1b[>4;2m"),
        ("formatOtherKeys 1", b"\x1b[>4;1m"),
        ("mouse 1000", b"\x1b[?1000h"),
        ("mouse 1002", b"\x1b[?1002h"),
        ("mouse 1003", b"\x1b[?1003h"),
        ("mouse 9 (x10)", b"\x1b[?9h"),
        ("mouse off", b"\x1b[?1003l\x1b[?9l"),
        ("focus reporting on", b"\x1b[?1004h"),
        ("focus reporting off", b"\x1b[?1004l"),
        ("focus reporting on again", b"\x1b[?1004h"),
        (
            "enter alt screen with own kitty state",
            b"\x1b[?1049h\x1b[>2u",
        ),
        ("DECSTR on alt", b"\x1b[!p"),
        ("leave alt screen", b"\x1b[?1049l"),
        ("alternate scroll on", b"\x1b[?1007h"),
        ("enter alt screen 1049", b"\x1b[?1049h"),
        ("alternate scroll off on alt", b"\x1b[?1007l"),
        ("leave alt screen 1049", b"\x1b[?1049l"),
        ("enter alt screen 47", b"\x1b[?47h"),
        ("leave alt screen 47", b"\x1b[?47l"),
        ("enter alt screen 1047", b"\x1b[?1047h"),
        ("leave alt screen 1047", b"\x1b[?1047l"),
        ("bracketed paste on", b"\x1b[?2004h"),
        ("bracketed paste off", b"\x1b[?2004l"),
        ("bracketed paste on again", b"\x1b[?2004h"),
        ("DECSTR clears bracketed paste", b"\x1b[!p"),
        ("VT52 enter", b"\x1b[?2l"),
        ("VT52 exit", b"\x1b<"),
        ("RIS", b"\x1bc"),
    ];
    for (what, bytes) in steps {
        term.process(bytes);
        assert_in_sync(&term, what);
    }
    term.process(b"\x1b[>1u");
    assert!(
        term.mode_mirror()
            .keyboard_mode()
            .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
    );
    term.process(b"\x1b[?1000h");
    assert_eq!(term.mode_mirror().mouse_mode(), MouseMode::Normal);
    term.process(b"\x1b[?1004h");
    assert!(term.mode_mirror().focus_reporting_enabled());
    term.process(b"\x1b[?1004l");
    assert!(!term.mode_mirror().focus_reporting_enabled());
    term.process(b"\x1b[?1007h\x1b[?1049h");
    assert!(term.mode_mirror().alternate_screen() && term.mode_mirror().alternate_scroll());
    term.process(b"\x1b[?1049l\x1b[?1007l");
    assert!(!term.mode_mirror().alternate_screen() && !term.mode_mirror().alternate_scroll());
    term.process(b"\x1b[?2004h");
    assert!(term.mode_mirror().bracketed_paste());
    term.process(b"\x1b[?2004l");
    assert!(!term.mode_mirror().bracketed_paste());
}

#[test]
fn every_host_mutator_refreshes_the_mirror() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>1u\x1b[?1000h");
    term.set_kitty_keyboard_enabled(false);
    assert_in_sync(&term, "set_kitty_keyboard_enabled(false)");
    assert!(
        !term
            .mode_mirror()
            .keyboard_mode()
            .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES),
        "capability off must drop the kitty bit from the lock-free word too"
    );
    term.set_kitty_keyboard_enabled(true);
    assert_in_sync(&term, "set_kitty_keyboard_enabled(true)");
    term.set_host_alternate_scroll(true);
    assert_in_sync(&term, "set_host_alternate_scroll(true)");
    assert!(term.mode_mirror().alternate_scroll());
    term.set_host_alternate_scroll(false);
    assert_in_sync(&term, "set_host_alternate_scroll(false)");
    term.process(b"\x1b[?1049h");
    assert!(term.mode_mirror().alternate_screen());
    assert!(term.leave_alternate_screen());
    assert_in_sync(&term, "leave_alternate_screen()");
    let mut config = crate::config::TerminalConfig::default();
    config.focus_reporting = true;
    let _ = term.apply_config(&config);
    assert!(term.focus_reporting_enabled(), "the host armed DEC 1004");
    assert_in_sync(&term, "apply_config(focus_reporting = true)");
    config.bracketed_paste = true;
    let _ = term.apply_config(&config);
    assert!(term.modes().bracketed_paste, "the host seeded DEC 2004");
    assert_in_sync(&term, "apply_config(bracketed_paste = true)");
    config.bracketed_paste = false;
    let _ = term.apply_config(&config);
    assert_in_sync(&term, "apply_config(bracketed_paste = false)");
    term.process(b"x");
    assert_in_sync(&term, "one output byte after apply_config");
    term.process(b"\x1b[?1004h");
    assert_in_sync(&term, "focus reporting armed");
    term.reset();
    assert_in_sync(&term, "reset()");
    assert_eq!(term.mode_mirror().mouse_mode(), MouseMode::None);
    assert!(
        !term.mode_mirror().focus_reporting_enabled(),
        "a reset disarms DEC 1004 in the lock-free word too"
    );

    let mut source = Terminal::new(24, 80);
    source.process(b"\x1b[>1u\x1b[?1002h\x1b[?1h\x1b[?1004h");
    let cp = source.checkpoint();
    let fresh = Terminal::from_checkpoint(&cp);
    assert_in_sync(&fresh, "from_checkpoint");
    assert_eq!(fresh.mode_mirror().mouse_mode(), MouseMode::ButtonEvent);
    assert!(
        fresh.mode_mirror().focus_reporting_enabled(),
        "a hydrated terminal carries DEC 1004 into the lock-free word"
    );
    let mut live = Terminal::new(24, 80);
    live.restore_checkpoint(&cp);
    assert_in_sync(&live, "restore_checkpoint");
    assert!(
        live.mode_mirror()
            .keyboard_mode()
            .contains(KeyboardMode::APP_CURSOR)
    );
}

#[test]
fn the_focus_arm_count_moves_on_each_program_enable_edge_only() {
    let mut term = Terminal::new(24, 80);
    let arms = |term: &Terminal| term.mode_mirror().focus_arms();
    assert_eq!(arms(&term), 0, "a fresh terminal has armed nothing");
    let steps: &[(&str, &[u8], u32)] = &[
        ("first enable", b"\x1b[?1004h", 1),
        ("a re-sent enable", b"\x1b[?1004h\x1b[?1004h", 1),
        ("a disable", b"\x1b[?1004l", 1),
        ("enable again", b"\x1b[?1004h", 2),
        ("off and on in one batch", b"\x1b[?1004l\x1b[?1004h", 3),
        (
            "XTSAVE, off, XTRESTORE",
            b"\x1b[?1004s\x1b[?1004l\x1b[?1004r",
            4,
        ),
        ("DECSTR turns it off", b"\x1b[!p", 4),
        ("RIS turns it off", b"\x1b[?1004h\x1bc", 5),
    ];
    for (what, bytes, want) in steps {
        term.process(bytes);
        assert_eq!(arms(&term), *want, "{what}");
        assert_in_sync(&term, what);
    }
    assert!(!term.focus_reporting_enabled(), "the reset left it off");
    let mut config = crate::config::TerminalConfig::default();
    config.focus_reporting = true;
    let _ = term.apply_config(&config);
    assert!(term.focus_reporting_enabled(), "the host armed DEC 1004");
    assert_eq!(
        arms(&term),
        5,
        "the host's arming is not the program's enable"
    );
    assert_in_sync(&term, "apply_config(focus_reporting = true)");
}
