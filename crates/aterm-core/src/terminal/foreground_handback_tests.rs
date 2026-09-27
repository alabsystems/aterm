// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for [`Terminal::foreground_handback`] — the 2026-09-25 "crashed tab"
//! (a Claude Code killed while it held the terminal left alt screen, kitty
//! flags, modifyOtherKeys, mouse tracking, focus, 2026 and a hidden cursor
//! armed under the shell that reclaimed it).

use aterm_types::keyboard::{Key, Modifiers, encode_key};
use aterm_types::mouse::{MouseEncoding, MouseMode};

use super::super::Terminal;
use crate::config::TerminalConfig;

fn ctrl7(t: &Terminal) -> Vec<u8> {
    encode_key(&Key::Character('7'), Modifiers::CTRL, t.keyboard_mode())
}

/// The owner's hand-typed reproduction of the incident, in either kitty order
/// (push after entering the alt screen, as Claude Code does, or before it).
/// Ends mid-OSC (a title write torn by the kill).
fn dead_claude(push_after: bool) -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(b"% claude\r\n");
    t.process(if push_after {
        b"\x1b[?1049h\x1b[>5u"
    } else {
        b"\x1b[>5u\x1b[?1049h"
    });
    t.process(
        b"\x1b[?1003h\x1b[?1006h\x1b[>4;2m\x1b[?2004h\x1b[?1004h\x1b[?25l\x1b[?2026h\x1b[5;10rFRAME\x1b]0;tr",
    );
    t
}

/// Everything the handback is responsible for, as one comparable value.
fn projection(t: &Terminal) -> impl PartialEq + core::fmt::Debug + use<> {
    let region = t.grid().scroll_region();
    let mut kitty = t.kitty_keyboard.snapshot();
    // The INACTIVE alt slot is never touched (plan R7); after an alt round trip
    // it reads `Some(0)` where a fresh terminal reads `None` — the same flags.
    if kitty.alt_saved_flags.is_some_and(|f| f.bits() == 0) {
        kitty.alt_saved_flags = None;
    }
    (
        *t.modes(),
        kitty,
        t.xterm_keyboard,
        (t.grid().cursor_row(), t.grid().cursor_col()),
        (region.top, region.bottom),
        t.parser_is_ground(),
    )
}

#[test]
fn foreground_handback_incident_is_handed_back_once_in_both_kitty_orders() {
    for push_after in [true, false] {
        let mut t = dead_claude(push_after);
        assert!(t.program_owns_terminal(), "the incident is evidence");
        assert!(!t.parser_is_ground(), "the kill tore the title write");
        assert_ne!(
            ctrl7(&t),
            ctrl7(&Terminal::new(24, 80)),
            "precondition: Ctrl+7 is CSI-u/mok"
        );
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_some(),
            "precondition: motion reports"
        );

        let h = t.foreground_handback().expect("the gate is open");
        // Claude Code pushes on the alt screen; the other order pushes on main
        // and 1049h parks the flags in main's saved slot.
        let kitty = if push_after { "kitty-alt" } else { "kitty" };
        for name in [
            "parser",
            "sync",
            "alt",
            kitty,
            "mouse",
            "mouse-encoding",
            "focus",
            "mok",
            "cursor",
        ] {
            assert!(
                h.reverted.contains(&name),
                "push_after={push_after}: {name} in {:?}",
                h.reverted
            );
        }
        assert_eq!(h.bytes.first(), Some(&0x18), "CAN first");
        assert!(!t.program_owns_terminal(), "the gate is closed afterwards");
        assert!(t.parser_is_ground());
        assert_eq!(
            ctrl7(&t),
            ctrl7(&Terminal::new(24, 80)),
            "Ctrl+7 is legacy 0x1f again"
        );
        assert!(
            t.encode_mouse_motion(0, 10, 5, 0).is_none(),
            "no motion reports"
        );
        let region = t.grid().scroll_region();
        assert_eq!((region.top, region.bottom), (0, 23), "full-screen region");
        assert_eq!(
            (t.grid().cursor_row(), t.grid().cursor_col()),
            (1, 0),
            "1049h's saved cursor"
        );
        assert_eq!(
            t.row_text(0).unwrap().trim_end(),
            "% claude",
            "row 0 intact"
        );
        assert_eq!(t.foreground_handback(), None, "a second call sends nothing");

        // The next alt-screen program starts with clean kitty flags.
        t.process(b"\x1b[?1049h");
        assert_eq!(
            t.kitty_keyboard_flags().bits(),
            0,
            "push_after={push_after}"
        );
        t.process(b"\x1b[?1049l");

        // The shell's own post-reclaim setup lands after the handback and wins.
        t.process(b"zsh: killed     claude\r\n% \x1b[?2004h");
        assert!(t.modes().bracketed_paste, "zle's 2004h survives");
        assert!(!t.program_owns_terminal());
    }
}

#[test]
fn foreground_handback_single_evidence_modes_each_open_the_gate() {
    let evidence: &[&[u8]] = &[
        b"\x1b[?1049h",
        b"\x1b[?47h",
        b"\x1b[?2l",
        b"\x1b[?1000h",
        b"\x1b[?1002h",
        b"\x1b[?1003h",
        b"\x1b[?25l",
        b"\x1b[?2026h",
        b"\x1b[?2048h",
        b"\x1b[?2031h",
        b"\x1b[?1004h",
        b"\x1b[>1u",
        b"\x1b[=1u",
        b"\x1b[>4;1m",
        b"\x1b[>4;1f",
    ];
    for seq in evidence {
        let mut t = Terminal::new(24, 80);
        t.process(seq);
        let label = String::from_utf8_lossy(seq);
        assert!(t.program_owns_terminal(), "{label:?} is evidence");
        assert_eq!(
            t.program_evidence().count_ones(),
            1,
            "{label:?} sets exactly one evidence bit"
        );
        assert!(t.foreground_handback().is_some(), "{label:?}");
        assert!(!t.program_owns_terminal(), "{label:?} closes the gate");
        assert_eq!(t.program_evidence(), 0, "{label:?}");
        assert_eq!(
            projection(&t),
            projection(&Terminal::new(24, 80)),
            "{label:?} back to power-on"
        );
    }
}

#[test]
fn foreground_handback_non_evidence_modes_are_left_alone() {
    let cases: &[(&[u8], fn(&Terminal) -> bool)] = &[
        (b"\x1b[?7l", |t| !t.modes().auto_wrap),
        (b"\x1b[?1h", |t| t.modes().application_cursor_keys),
        (b"\x1b=", |t| t.modes().application_keypad),
        (b"\x1b(0", |t| {
            t.charset.g0 != aterm_types::charset::CharacterSet::Ascii
        }),
        (b"\x1b[4h", |t| t.modes().insert_mode),
        (b"\x1b[20h", |t| t.modes().new_line_mode),
        (b"\x1b[?5h", |t| t.modes().reverse_video),
        (b"\x1b[?1007h", |t| t.modes().alternate_scroll),
        (b"\x1b[?67h", |t| t.modes().backarrow_sends_bs),
        (b"\x1b[?1036h", |t| t.modes().meta_send_escape),
        (b"\x1b[?1006h", |t| {
            t.modes().mouse_encoding == MouseEncoding::Sgr
        }),
    ];
    for (seq, survives) in cases {
        let mut t = Terminal::new(24, 80);
        t.process(seq);
        let label = String::from_utf8_lossy(seq);
        assert!(survives(&t), "precondition {label:?}");
        assert!(!t.program_owns_terminal(), "{label:?} is not evidence");
        assert_eq!(t.foreground_handback(), None, "{label:?} sends nothing");
        assert!(survives(&t), "{label:?} survives");
    }
}

#[test]
fn foreground_handback_hidden_cursor_takes_the_non_evidence_modes_with_it() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?7l\x1b[?25l\x1b(0\x1b[?1h");
    let h = t.foreground_handback().expect("hidden cursor is evidence");
    assert!(
        h.reverted.contains(&"wrap") && h.reverted.contains(&"cursor"),
        "{:?}",
        h.reverted
    );
    assert!(t.modes().auto_wrap && t.modes().cursor_visible);
    assert!(!t.modes().application_cursor_keys);
    assert_eq!(t.charset.g0, aterm_types::charset::CharacterSet::Ascii);
}

#[test]
fn foreground_handback_torn_sequence_alone_gets_only_can() {
    let torn: &[&[u8]] = &[
        b"\x1b]8;;http://example",
        b"\x1b[12;",
        b"\x1bP0;0;0q#0;2;0;0;0",
        b"\x1b_Ga=T,f=100;",
        b"\x1b",
    ];
    for seq in torn {
        let mut t = Terminal::new(24, 80);
        t.process(seq);
        let label = String::from_utf8_lossy(seq);
        assert!(!t.parser_is_ground(), "precondition {label:?}");
        assert!(!t.program_owns_terminal(), "{label:?}");
        let h = t.foreground_handback().expect("CAN");
        assert_eq!(h.bytes, vec![0x18], "{label:?}");
        assert_eq!(h.reverted, vec!["parser"]);
        assert!(t.parser_is_ground());
        t.process(b"% ");
        assert_eq!(
            t.row_text(0).unwrap().trim_end(),
            "%",
            "{label:?}: next text prints at column 0"
        );
        assert_eq!(t.grid().cursor_col(), 2, "{label:?}");
    }
}

#[test]
fn foreground_handback_leaves_vt52_before_any_csi() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?1000h\x1b[?25l\x1b[?2l");
    assert!(t.modes().vt52_mode);
    let h = t.foreground_handback().expect("owned");
    assert!(
        h.bytes.starts_with(b"\x1b<"),
        "{:?}",
        String::from_utf8_lossy(&h.bytes)
    );
    assert!(!t.modes().vt52_mode && t.modes().cursor_visible);
    assert_eq!(t.modes().mouse_mode, MouseMode::None);
}

#[test]
fn foreground_handback_clears_margins_and_keeps_the_cursor() {
    // DECOM + region on the main screen.
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?1000h\x1b[3;20r\x1b[?6h\x1b[5;7H");
    let before = (t.grid().cursor_row(), t.grid().cursor_col());
    let h = t.foreground_handback().expect("owned");
    assert!(h.reverted.contains(&"origin") && h.reverted.contains(&"region"));
    assert!(!t.modes().origin_mode);
    assert_eq!((t.grid().cursor_row(), t.grid().cursor_col()), before);
    let region = t.grid().scroll_region();
    assert_eq!((region.top, region.bottom), (0, 23));

    // DECSTBM set on alt is copied onto main by 1049l (handler_dec).
    let mut t = Terminal::new(24, 80);
    t.process(b"ab\r\ncd\x1b[?1049h\x1b[?25l\x1b[5;10r\x1b[?1049l");
    let region = t.grid().scroll_region();
    assert_eq!(
        (region.top, region.bottom),
        (4, 9),
        "precondition: the region leaked to main"
    );
    let before = (t.grid().cursor_row(), t.grid().cursor_col());
    t.foreground_handback().expect("hidden cursor");
    let region = t.grid().scroll_region();
    assert_eq!((region.top, region.bottom), (0, 23));
    assert_eq!((t.grid().cursor_row(), t.grid().cursor_col()), before);
}

#[test]
fn foreground_handback_targets_the_host_configured_values() {
    let config = TerminalConfig {
        focus_reporting: true,
        bracketed_paste: true,
        auto_wrap: true,
        ..TerminalConfig::default()
    };
    let mut t = Terminal::new(24, 80);
    t.apply_config(&config);
    assert!(t.modes().focus_reporting && t.modes().bracketed_paste);
    assert!(
        !t.program_owns_terminal(),
        "host-configured 1004 is not evidence"
    );
    assert_eq!(t.foreground_handback(), None);

    // A program turned all three off and hid the cursor.
    t.process(b"\x1b[?1004l\x1b[?2004l\x1b[?7l\x1b[?25l");
    let h = t.foreground_handback().expect("owned");
    assert!(
        h.reverted.contains(&"focus") && h.reverted.contains(&"paste"),
        "{:?}",
        h.reverted
    );
    assert!(t.modes().focus_reporting, "back ON: the host configured it");
    assert!(t.modes().bracketed_paste);
    assert!(t.modes().auto_wrap);
}

#[test]
fn foreground_handback_replays_to_the_live_state() {
    for push_after in [true, false] {
        let mut live = dead_claude(push_after);
        let h = live.foreground_handback().expect("owned");
        let mut replay = dead_claude(push_after);
        replay.process(&h.bytes);
        assert_eq!(
            projection(&replay),
            projection(&live),
            "push_after={push_after}"
        );
    }
}

/// 2026-09-25 review: every handback used to emit `?1007l` whenever 1007 was
/// on, so aterm-gui's default-ON alternate scroll (Audit M5) was lost at the
/// first handback and the wheel stopped scrolling `less` for good. 1007 now
/// goes back to the HOST's value.
#[test]
fn foreground_handback_restores_alternate_scroll_to_the_host_value() {
    // The host turned 1007 on; a program that never touched it died hidden.
    let mut t = Terminal::new(24, 80);
    t.set_host_alternate_scroll(true);
    t.process(b"\x1b[?25l");
    let h = t.foreground_handback().expect("hidden cursor is evidence");
    assert!(
        !h.reverted.contains(&"alternate-scroll"),
        "{:?}",
        h.reverted
    );
    assert!(
        !h.bytes.windows(8).any(|w| w == b"\x1b[?1007l"),
        "{:?}",
        String::from_utf8_lossy(&h.bytes)
    );
    assert!(t.modes().alternate_scroll, "the host's ON survives");

    // A program switched it off and died: it comes back ON.
    t.process(b"\x1b[?1007l\x1b[?1049h");
    let h = t.foreground_handback().expect("alt screen");
    assert!(h.reverted.contains(&"alternate-scroll"), "{:?}", h.reverted);
    assert!(t.modes().alternate_scroll);

    // With no host baseline the power-on OFF is the target, as before.
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[?1007h\x1b[?25l");
    t.foreground_handback().expect("hidden cursor");
    assert!(!t.modes().alternate_scroll);
}

/// `restore_modes = false`: the program that lost the terminal is gone, but
/// the modes in force belong to one that still runs (the host's call). Only a
/// torn sequence is cancelled; every mode stays.
#[test]
fn foreground_handback_scoped_without_modes_only_cancels_a_torn_sequence() {
    let mut t = dead_claude(true);
    let before = t.program_evidence();
    assert_ne!(before, 0);
    let h = t
        .foreground_handback_scoped(false)
        .expect("torn title write");
    assert_eq!(h.bytes, vec![0x18]);
    assert_eq!(h.reverted, vec!["parser"]);
    assert!(t.parser_is_ground());
    assert_eq!(t.program_evidence(), before, "every mode is kept");
    assert!(t.is_alternate_screen());
    assert_eq!(
        t.foreground_handback_scoped(false),
        None,
        "at ground with modes out of scope there is nothing to send"
    );
}

/// Kitty flags pushed on the MAIN screen before `?1049h` are the program's
/// input bit even while the alt screen hides them (final review of the
/// handback, 2026-09-25). A program that pushed, entered the alt screen and
/// died with no other input mode was reported as owning display bits only,
/// so it was not handed back; the flags came back live when the shell left
/// the alt screen with a builtin, and Ctrl+7 reached the prompt as CSI-u.
/// NEGATIVE CONTROL: the same program that never pushed owns no input bit.
#[test]
fn foreground_handback_main_screen_kitty_flags_under_the_alt_screen_are_evidence() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[>5u\x1b[?1049h\x1b[?25l");
    assert_ne!(
        t.program_evidence() & super::evidence::INPUT,
        0,
        "the parked main-screen flags are an input bit"
    );
    assert!(t.foreground_handback().is_some(), "handed back");
    assert!(!t.modes().alternate_screen);
    assert_eq!(ctrl7(&t), vec![0x1f], "Ctrl+7 is legacy at the prompt");

    let mut plain = Terminal::new(24, 80);
    plain.process(b"\x1b[?1049h\x1b[?25l");
    assert_eq!(
        plain.program_evidence() & super::evidence::INPUT,
        0,
        "no push, no input bit: a bare smcup/civis is display only"
    );
}
