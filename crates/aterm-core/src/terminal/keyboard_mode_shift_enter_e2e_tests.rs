// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shift+Enter end to end.
use crate::terminal::Terminal;
use aterm_types::keyboard::{
    Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout,
};

pub(super) fn enc_enter(term: &Terminal, mods: Modifiers) -> Vec<u8> {
    encode_key_with_layout(
        &Key::Named(NamedKey::Enter),
        mods,
        term.keyboard_mode(),
        KeyEventType::Press,
        None,
    )
}

#[test]
fn legacy_shift_enter_is_lf_aterm_imposes_newline() {
    let term = Terminal::new(24, 80);
    assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
    assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
    assert_eq!(enc_enter(&term, Modifiers::CTRL), vec![0x0d]);
    assert_eq!(enc_enter(&term, Modifiers::ALT), vec![0x1b, 0x0d]);
}

#[test]
fn push_disambiguate_then_shift_enter_is_csi_u() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>1u");
    assert!(
        term.keyboard_mode()
            .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES),
        "push must enable disambiguate in the derived keyboard mode"
    );
    assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
    assert_eq!(
        enc_enter(&term, Modifiers::SHIFT),
        b"\x1b[13;2u",
        "Shift+Enter under disambiguate must be ESC[13;2u — what Claude reads as a newline"
    );
    term.process(b"\x1b[?u");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1u");
}

#[test]
fn codex_report_event_types_suppresses_predictive_echo() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>7u");
    let mode = term.keyboard_mode();
    assert!(mode.contains(KeyboardMode::REPORT_EVENT_TYPES));
    assert!(!mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(
        term.kitty_suppresses_predictive_echo(),
        "an app-owned Codex composer must never receive terminal ghost glyphs"
    );

    term.process(b"\x1b[=1u");
    assert!(!term.kitty_suppresses_predictive_echo());
}

#[test]
fn functional_key_reporting_tracks_what_the_encoder_actually_emits() {
    let menu = |term: &Terminal| {
        encode_key_with_layout(
            &Key::Named(NamedKey::ContextMenu),
            Modifiers::empty(),
            term.keyboard_mode(),
            KeyEventType::Press,
            None,
        )
    };
    let term = Terminal::new(24, 80);
    assert!(!term.kitty_reports_functional_keys());
    assert!(
        menu(&term).is_empty(),
        "legacy has no sequence for the Menu key"
    );

    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>1u");
    assert!(term.kitty_reports_functional_keys());
    assert_eq!(
        menu(&term),
        b"\x1b[57363u",
        "…and the app really does receive 57363"
    );

    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>8u");
    assert!(term.kitty_reports_functional_keys());
    assert!(term.kitty_report_all_keys());

    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>2u");
    assert!(!term.kitty_reports_functional_keys());
    assert!(
        menu(&term).is_empty(),
        "event-types-only still cannot deliver the Menu key"
    );
}

#[test]
fn modify_other_keys_level2_distinguishes_shift_enter() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>4;2m");
    assert_eq!(
        enc_enter(&term, Modifiers::SHIFT),
        b"\x1b[27;2;13~",
        "Shift+Enter under modifyOtherKeys L2 must be ESC[27;2;13~"
    );
}

#[test]
fn push_flags_over_255_are_masked_not_saturated() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>256u");
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?0u",
        "undefined high bits must be masked out of the pushed flags"
    );
    term.process(b"\x1b[>289u");
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?1u",
        "push masks to the five defined flags (0b1_1111), clearing bit 5 (32) as well as bit 8"
    );
    term.process(b"\x1b[=290;1u");
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?2u",
        "set masks to the five defined flags (0b1_1111)"
    );
}

#[test]
fn decstr_resets_only_the_current_screens_kitty_state() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[>1u");
    term.process(b"\x1b[?1049h");
    term.process(b"\x1b[>2u");
    term.process(b"\x1b[!p");
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?0u",
        "DECSTR must clear the ACTIVE screen's kitty flags"
    );
    term.process(b"\x1b[?1049l");
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?1u",
        "the inactive (main) screen's kitty state must survive an alt-screen DECSTR"
    );
}

fn conpty_term() -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.set_win32_input_enabled(true);
    term
}

fn win32_enter_pair(uc: u32, cs: u32) -> Vec<u8> {
    format!("\x1b[13;28;{uc};1;{cs};1_\x1b[13;28;{uc};0;{cs};1_").into_bytes()
}

#[test]
fn conpty_9001_turns_enter_chords_into_win32_records_and_9001l_turns_them_back() {
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    let mode = term.keyboard_mode();
    assert!(mode.contains(KeyboardMode::WIN32_INPUT));
    assert!(
        !mode.intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS),
        "9001 must not masquerade as a kitty negotiation"
    );
    assert!(
        term.mode_mirror()
            .keyboard_mode()
            .contains(KeyboardMode::WIN32_INPUT)
    );

    assert_eq!(
        enc_enter(&term, Modifiers::SHIFT),
        win32_enter_pair(13, 0x10)
    );
    assert_eq!(
        enc_enter(&term, Modifiers::CTRL),
        win32_enter_pair(10, 0x08)
    );
    assert_eq!(
        enc_enter(&term, Modifiers::CTRL | Modifiers::SHIFT),
        win32_enter_pair(10, 0x18)
    );
    assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
    assert_eq!(enc_enter(&term, Modifiers::ALT), vec![0x1b, 0x0d]);

    term.process(b"\x1b[?9001l");
    assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
    assert_eq!(enc_enter(&term, Modifiers::CTRL), vec![0x0d]);
}

#[test]
fn conpty_9001_release_of_an_enter_chord_emits_no_second_key_up() {
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    for mods in [
        Modifiers::SHIFT,
        Modifiers::CTRL,
        Modifiers::CTRL | Modifiers::SHIFT,
    ] {
        assert!(
            encode_key_with_layout(
                &Key::Named(NamedKey::Enter),
                mods,
                term.keyboard_mode(),
                KeyEventType::Release,
                None,
            )
            .is_empty(),
            "{mods:?} release must not produce a second key-up"
        );
    }
}

#[test]
fn conpty_9001_never_leaks_into_the_kitty_query_or_the_kitty_stack() {
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    term.process(b"\x1b[?u");
    assert_eq!(
        term.take_response().unwrap_or_default(),
        b"\x1b[?0u",
        "9001 is a host negotiation, not a kitty flag: the app must read ?0u"
    );
    term.process(b"\x1b[>1u");
    term.process(b"\x1b[?u");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1u");
    assert!(
        term.keyboard_mode()
            .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES)
    );
    assert_eq!(
        enc_enter(&term, Modifiers::SHIFT),
        win32_enter_pair(13, 0x10)
    );
    assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
    term.process(b"\x1b[<u");
    term.process(b"\x1b[?u");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?0u");
    assert!(term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    assert!(
        !term
            .keyboard_mode()
            .intersects(KeyboardMode::KITTY_PROTOCOL_FLAGS)
    );
    assert_eq!(
        enc_enter(&term, Modifiers::SHIFT),
        win32_enter_pair(13, 0x10)
    );
}

#[test]
fn conpty_9001_changes_no_application_facing_projection() {
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    assert!(
        !term.kitty_suppresses_predictive_echo(),
        "a ConPTY shell still gets predictive echo: 9001 is not an app-owned composer"
    );
    assert!(!term.kitty_reports_functional_keys());
    assert!(!term.kitty_report_all_keys());
    assert_eq!(term.kitty_keyboard_flags().bits(), 0);
}

#[test]
fn conpty_9001_sends_escape_as_a_record_so_it_survives_the_first_chord() {
    let key = |term: &Terminal, key: Key, mods: Modifiers| {
        encode_key_with_layout(
            &key,
            mods,
            term.mode_mirror().keyboard_mode(),
            KeyEventType::Press,
            None,
        )
    };
    let esc = || Key::Named(NamedKey::Escape);
    let mut term = conpty_term();
    term.process(b"\x1b[?9001h");
    assert_eq!(
        key(&term, esc(), Modifiers::empty()),
        b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_"
    );
    assert_eq!(
        key(&term, Key::Character('['), Modifiers::CTRL),
        b"\x1b[219;26;27;1;8;1_\x1b[219;26;27;0;8;1_"
    );
    assert_eq!(
        key(&term, Key::Character('['), Modifiers::ALT),
        b"\x1b[219;26;91;1;2;1_\x1b[219;26;91;0;2;1_"
    );
    assert_eq!(key(&term, Key::Character('a'), Modifiers::ALT), b"\x1ba");
    term.process(b"\x1b[?9001l");
    assert_eq!(key(&term, esc(), Modifiers::empty()), b"\x1b");
    assert_eq!(key(&term, Key::Character('['), Modifiers::ALT), b"\x1b[");
}

#[test]
fn program_output_cannot_turn_on_win32_records_without_a_console_host() {
    let key = |term: &Terminal, key: Key, mods: Modifiers| {
        encode_key_with_layout(
            &key,
            mods,
            term.mode_mirror().keyboard_mode(),
            KeyEventType::Press,
            None,
        )
    };
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?9001h");
    assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    assert!(
        !term
            .mode_mirror()
            .keyboard_mode()
            .contains(KeyboardMode::WIN32_INPUT)
    );
    assert_eq!(
        key(&term, Key::Named(NamedKey::Escape), Modifiers::empty()),
        b"\x1b"
    );
    assert_eq!(key(&term, Key::Character('['), Modifiers::CTRL), b"\x1b");
    assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
    assert_eq!(enc_enter(&term, Modifiers::CTRL), vec![0x0d]);
    term.process(b"\x1b[?9001$p");
    assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?9001;0$y");
}

#[test]
fn the_console_host_posture_follows_the_resize_policy_both_ways() {
    use crate::grid::ResizePolicy;
    let mut conpty = Terminal::new(24, 80);
    conpty.set_console_host_posture(ResizePolicy::ConPty);
    conpty.process(b"\x1b[?u");
    assert_eq!(
        conpty.take_response(),
        None,
        "no kitty offer behind conhost"
    );
    conpty.process(b"\x1b[?9001h");
    assert!(conpty.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    assert_eq!(
        enc_enter(&conpty, Modifiers::SHIFT),
        win32_enter_pair(13, 0x10)
    );

    let mut native = Terminal::from_checkpoint(&conpty.checkpoint());
    assert!(
        native.keyboard_mode().contains(KeyboardMode::WIN32_INPUT),
        "control: the checkpoint carried the console host's bits"
    );
    native.set_console_host_posture(ResizePolicy::Native);
    assert!(!native.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    assert!(
        !native
            .mode_mirror()
            .keyboard_mode()
            .contains(KeyboardMode::WIN32_INPUT)
    );
    assert_eq!(enc_enter(&native, Modifiers::SHIFT), vec![0x0a]);
    native.process(b"\x1b[?9001h\x1b[?u");
    assert!(!native.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    assert_eq!(native.take_response().unwrap_or_default(), b"\x1b[?0u");
}

#[test]
fn the_openconsole_host_bit_rides_with_win32_input_mode() {
    let oc = |t: &Terminal| {
        (
            t.keyboard_mode().contains(KeyboardMode::OPENCONSOLE_HOST),
            t.mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::OPENCONSOLE_HOST),
        )
    };
    let mut term = conpty_term();
    term.set_win32_input_openconsole(true);
    assert_eq!(oc(&term), (false, false), "nothing before the host's 9001");
    term.process(b"\x1b[?9001h");
    assert_eq!(oc(&term), (true, true));
    term.process(b"\x1bc");
    assert_eq!(oc(&term), (false, false), "RIS clears 9001 itself");
    term.process(b"\x1b[?9001h");
    assert_eq!(oc(&term), (true, true), "but keeps the host policy");
    term.set_win32_input_openconsole(false);
    assert_eq!(oc(&term), (false, false));
    assert!(term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
    let mut native = Terminal::new(24, 80);
    native.set_win32_input_openconsole(true);
    native.process(b"\x1b[?9001h");
    assert_eq!(oc(&native), (false, false));
}
