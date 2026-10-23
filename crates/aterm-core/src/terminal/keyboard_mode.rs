// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Keyboard mode derivation from terminal state.

use aterm_types::keyboard::KeyboardMode;

use super::{KittyKeyboardFlags, Terminal, TerminalModes, XtermKeyboardState};

/// Derive `KeyboardMode` from terminal modes, Kitty flags and xterm state.
#[inline]
#[must_use]
pub(crate) fn keyboard_mode_from_state(
    modes: &TerminalModes,
    kitty: KittyKeyboardFlags,
    xterm: XtermKeyboardState,
) -> KeyboardMode {
    let mut km = aterm_types::keyboard::TermMode::from_keyboard_state(
        modes.application_cursor_keys,
        modes.application_keypad,
        modes.vt52_mode,
        kitty,
        xterm,
    )
    .to_keyboard_mode();
    if modes.backarrow_sends_bs {
        km.insert(KeyboardMode::BACKARROW_SENDS_BS);
    }
    if !modes.alt_send_escape {
        km.insert(KeyboardMode::ALT_NO_ESC);
    }
    if modes.meta_send_escape {
        km.insert(KeyboardMode::META_SENDS_ESC);
    }
    if !modes.special_modifiers {
        km.insert(KeyboardMode::NO_SPECIAL_MODIFIERS);
    }
    if modes.win32_input_active() {
        km.insert(KeyboardMode::WIN32_INPUT);
        if modes.win32_input_openconsole {
            km.insert(KeyboardMode::OPENCONSOLE_HOST);
        }
    }
    km
}

/// The xterm modifyOtherKeys / formatOtherKeys state (XTMODKEYS / XTFMTKEYS) a session offers.
#[inline]
pub(crate) const fn offered_xterm_keyboard(
    modes: &TerminalModes,
    xterm: XtermKeyboardState,
) -> XtermKeyboardState {
    if modes.win32_input_enabled {
        XtermKeyboardState::new()
    } else {
        xterm
    }
}

impl Terminal {
    /// Get the keyboard encoding mode flags for this terminal.
    #[inline]
    #[must_use]
    pub fn keyboard_mode(&self) -> KeyboardMode {
        let kitty = if self.modes().kitty_keyboard_enabled {
            self.kitty_keyboard_flags()
        } else {
            KittyKeyboardFlags::none()
        };
        let xterm = offered_xterm_keyboard(self.modes(), *self.xterm_keyboard());
        keyboard_mode_from_state(self.modes(), kitty, xterm)
    }

    /// Whether the kitty keyboard protocol's `REPORT_ALL_KEYS_AS_ESC` (0b1000) enhancement is active.
    #[inline]
    #[must_use]
    pub fn kitty_report_all_keys(&self) -> bool {
        self.keyboard_mode()
            .contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }

    /// Whether the active Kitty keyboard mode reports functional keys that have no legacy escape form (the Menu key `57363`, F13-F35, the media cluster).
    #[inline]
    #[must_use]
    pub fn kitty_reports_functional_keys(&self) -> bool {
        self.keyboard_mode()
            .intersects(KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }

    /// Whether the active keyboard mode sends Ctrl-letter chords as escape sequences rather than C0 bytes.
    #[inline]
    #[must_use]
    pub fn control_chords_escaped(&self) -> bool {
        let mode = self.keyboard_mode();
        mode.intersects(KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
            || mode.xterm_modify_other_keys_level() >= 2
    }

    /// Whether the active Kitty keyboard mode makes predictive local echo unsafe.
    #[inline]
    #[must_use]
    pub fn kitty_suppresses_predictive_echo(&self) -> bool {
        self.keyboard_mode()
            .intersects(KeyboardMode::REPORT_EVENT_TYPES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }
}

#[cfg(test)]
mod shift_enter_e2e_tests {
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
        term.process(b"\x1b[=257;1u");
        term.process(b"\x1b[?u");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1u");
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
}

#[cfg(test)]
mod kitty_capability_toggle_tests {
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
}

#[cfg(test)]
mod console_host_modify_other_keys_tests {
    //! Behind a Windows console host a native console program's `CSI > 4 ; 2 m`
    //! reaches the terminal (OpenConsole and the inbox conhost both forward it),
    //! and conhost, which reads the key bytes, has no reader for the
    //! `CSI 27 ; m ; c ~` chords it would ask for: PSReadLine got nothing for
    //! Shift+A, Ctrl+A/E/K or Alt+B until the level was reset (measured
    //! 2026-10-18).
    use crate::grid::ResizePolicy;
    use crate::terminal::Terminal;
    use crate::terminal::program_evidence;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey, encode_key_with_layout};

    fn chords(term: &Terminal) -> Vec<Vec<u8>> {
        let c = |k: char| Key::Character(k);
        let n = Key::Named;
        let cases = [
            (c('a'), Modifiers::CTRL),
            (c('e'), Modifiers::CTRL),
            (c('k'), Modifiers::CTRL),
            (c('A'), Modifiers::SHIFT),
            (c('@'), Modifiers::SHIFT),
            (c('a'), Modifiers::ALT),
            (c('b'), Modifiers::ALT),
            (c('a'), Modifiers::CTRL | Modifiers::ALT),
            (c('K'), Modifiers::CTRL | Modifiers::SHIFT),
            (c('i'), Modifiers::CTRL),
            (n(NamedKey::Tab), Modifiers::SHIFT),
            (n(NamedKey::Tab), Modifiers::CTRL),
            (n(NamedKey::Enter), Modifiers::ALT),
        ];
        cases
            .iter()
            .map(|(key, mods)| {
                encode_key_with_layout(
                    key,
                    *mods,
                    term.mode_mirror().keyboard_mode(),
                    KeyEventType::Press,
                    None,
                )
            })
            .collect()
    }

    fn session(policy: ResizePolicy) -> Terminal {
        let mut term = Terminal::new(24, 80);
        term.set_console_host_posture(policy);
        term.process(b"\x1b[?9001h");
        term
    }

    fn mok_query(term: &mut Terminal) -> Vec<u8> {
        term.process(b"\x1b[?4m");
        term.take_response().unwrap_or_default()
    }

    #[test]
    fn a_console_host_session_ignores_modify_other_keys_and_reads_level_0() {
        let untouched = chords(&session(ResizePolicy::ConPty));
        assert_eq!(untouched[0], [0x01], "control: legacy Ctrl+A is ^A");
        assert_eq!(untouched[3], b"A", "control: legacy Shift+A is A");

        let mut native = session(ResizePolicy::Native);
        native.process(b"\x1b[>4;2m");
        assert_eq!(mok_query(&mut native), b"\x1b[>4;2m");
        assert_eq!(chords(&native)[0], b"\x1b[27;5;97~");

        let mut term = session(ResizePolicy::ConPty);
        let requests: [&[u8]; 4] = [b"\x1b[>4;2m", b"\x1b[>4;1m", b"\x1b[>4;1f", b"\x1b[>4n"];
        for level in requests {
            term.process(level);
            assert_eq!(chords(&term), untouched, "{level:?}");
            assert_eq!(mok_query(&mut term), b"\x1b[>4;0m", "{level:?}");
        }
        term.process(b"\x1b[?4g");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[>4;0f");
        assert_eq!(
            term.keyboard_mode().xterm_modify_other_keys_level(),
            0,
            "`ctl modes` reads the level the encoder uses"
        );
        assert_eq!(
            term.take_evidence_asserted()
                & (program_evidence::MODIFY_OTHER_KEYS | program_evidence::FORMAT_OTHER_KEYS),
            0
        );
        assert!(!term.program_owns_terminal());
    }

    #[test]
    fn a_level_negotiated_before_the_console_host_posture_stops_reaching_the_encoder() {
        let mut term = session(ResizePolicy::Native);
        term.process(b"\x1b[>4;2m");
        let negotiated = chords(&term);
        term.set_console_host_posture(ResizePolicy::ConPty);
        term.process(b"\x1b[?9001h");
        assert_eq!(chords(&term), chords(&session(ResizePolicy::ConPty)));
        assert_eq!(mok_query(&mut term), b"\x1b[>4;0m");
        term.set_console_host_posture(ResizePolicy::Native);
        assert_eq!(chords(&term), negotiated);
    }
}

#[cfg(test)]
mod control_chord_tests {
    //! `control_chords_escaped` is the encoder's own answer.
    use crate::terminal::Terminal;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, encode_key_with_layout};

    fn chord(term: &Terminal, c: char) -> Vec<u8> {
        encode_key_with_layout(
            &Key::Character(c),
            Modifiers::CTRL,
            term.keyboard_mode(),
            KeyEventType::Press,
            None,
        )
    }

    #[test]
    fn control_chords_escaped_is_what_the_encoder_sends() {
        let negotiations: [(&str, &[u8], bool); 7] = [
            ("legacy", b"", false),
            ("kitty disambiguate", b"\x1b[>1u", true),
            ("kitty report all keys", b"\x1b[>8u", true),
            ("kitty event types alone", b"\x1b[>2u", false),
            ("modifyOtherKeys 1", b"\x1b[>4;1m", false),
            ("modifyOtherKeys 2", b"\x1b[>4;2m", true),
            ("modifyOtherKeys 2 reset", b"\x1b[>4;2m\x1b[>4;0m", false),
        ];
        for (label, bytes, escaped) in negotiations {
            let mut term = Terminal::new(4, 20);
            term.process(bytes);
            assert_eq!(term.control_chords_escaped(), escaped, "{label}");
            assert_eq!(
                chord(&term, 'a') != [0x01] && chord(&term, 'e') != [0x05],
                escaped,
                "{label}: the encoder agrees ({:?})",
                String::from_utf8_lossy(&chord(&term, 'a'))
            );
        }
    }
}
