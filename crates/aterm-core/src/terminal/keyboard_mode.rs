// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Keyboard mode derivation from terminal state.
//!
//! Provides a bridge-agnostic conversion from `Terminal` accessors
//! (`TerminalModes`, `KittyKeyboardFlags`, `XtermKeyboardState`) into
//! `aterm_types::keyboard::KeyboardMode`.

use aterm_types::keyboard::KeyboardMode;

use super::{KittyKeyboardFlags, Terminal, TerminalModes, XtermKeyboardState};

/// Derive `KeyboardMode` from terminal modes, Kitty flags and xterm state:
/// `aterm_types::keyboard::TermMode::from_keyboard_state` (the single source of
/// the keyboard projection) plus the legacy-encoding modes it does not cover.
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
    // DECBKM (mode 67) is outside the TermMode projection.
    if modes.backarrow_sends_bs {
        km.insert(KeyboardMode::BACKARROW_SENDS_BS);
    }
    // xterm private modes 1035/1036/1039, modeled so the empty mode keeps the
    // default encoding:
    //   - 1039 altSendsEscape: negative flag, reset suppresses the Alt ESC.
    //   - 1036 metaSendsEscape: positive flag, set adds the Meta ESC.
    //   - 1035 numLock: negative flag, reset strips the NumLock modifier.
    if !modes.alt_send_escape {
        km.insert(KeyboardMode::ALT_NO_ESC);
    }
    if modes.meta_send_escape {
        km.insert(KeyboardMode::META_SENDS_ESC);
    }
    if !modes.special_modifiers {
        km.insert(KeyboardMode::NO_SPECIAL_MODIFIERS);
    }
    // ConPTY win32-input-mode (DEC 9001), only where the host says a console
    // host is there (`win32_input_active`). Never in the kitty flag word and not
    // gated on `kitty_keyboard_enabled`: conhost asked for it and reads the
    // bytes, so in the encoder it outranks a kitty push (see
    // `KeyboardMode::WIN32_INPUT`).
    if modes.win32_input_active() {
        km.insert(KeyboardMode::WIN32_INPUT);
    }
    km
}

impl Terminal {
    /// Get the keyboard encoding mode flags for this terminal.
    ///
    /// Returns a bridge-agnostic `KeyboardMode` that can be passed directly
    /// to `aterm_types::keyboard::encode_key*` functions.
    #[must_use]
    pub fn keyboard_mode(&self) -> KeyboardMode {
        // Capability off ⇒ derive as if the protocol is unsupported: no kitty
        // bits even if flags were negotiated before the host disabled it.
        let kitty = if self.modes().kitty_keyboard_enabled {
            self.kitty_keyboard_flags()
        } else {
            KittyKeyboardFlags::none()
        };
        keyboard_mode_from_state(self.modes(), kitty, *self.xterm_keyboard())
    }

    /// Whether the kitty keyboard protocol's `REPORT_ALL_KEYS_AS_ESC` (0b1000)
    /// enhancement is active: every key reaches the application as a CSI-u
    /// report, so it never receives echoing text. A narrow read-only projection
    /// of [`Self::keyboard_mode`], with the same capability-off gating.
    #[must_use]
    pub fn kitty_report_all_keys(&self) -> bool {
        self.keyboard_mode()
            .contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }

    /// Whether the active Kitty keyboard mode reports functional keys that have
    /// no legacy escape form (the Menu key `57363`, F13-F35, the media cluster).
    /// Legacy modes encode them to zero bytes; only `DISAMBIGUATE_ESC_CODES` and
    /// `REPORT_ALL_KEYS_AS_ESC` route a bare `Key::Named` press through the CSI-u
    /// encoder (`should_encode_kitty_event`).
    ///
    /// For chrome deference: a host gesture must not claim a key the front
    /// application negotiated for, and may take one nobody receives. A read-only
    /// projection of [`Self::keyboard_mode`]; it never feeds an encoder.
    #[must_use]
    pub fn kitty_reports_functional_keys(&self) -> bool {
        self.keyboard_mode()
            .intersects(KeyboardMode::DISAMBIGUATE_ESC_CODES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }

    /// Whether the active Kitty keyboard mode makes predictive local echo unsafe:
    /// `REPORT_ALL_KEYS_AS_ESC` routes even plain text through CSI-u, and
    /// `REPORT_EVENT_TYPES` marks an application-owned composer (Codex on the
    /// main screen repaints its own input line, so guesses would duplicate its
    /// text until the predictor timeout). A narrow read-only projection for
    /// display-side callers.
    #[must_use]
    pub fn kitty_suppresses_predictive_echo(&self) -> bool {
        self.keyboard_mode()
            .intersects(KeyboardMode::REPORT_EVENT_TYPES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }
}

#[cfg(test)]
mod shift_enter_e2e_tests {
    //! Shift+Enter end to end: drive the negotiation an app performs, then check
    //! the derived `KeyboardMode` and the bytes the encoder emits.
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
        // aterm's input policy forces a usable Shift+Enter even with no protocol and
        // no faked identity: plain Enter is CR, Shift+Enter is LF (0x0a) — Claude /
        // readline / vim read LF as insert-newline. Ctrl+Enter stays CR; Alt+Enter is
        // Meta-Enter (ESC CR).
        assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
        assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
        assert_eq!(enc_enter(&term, Modifiers::CTRL), vec![0x0d]);
        assert_eq!(enc_enter(&term, Modifiers::ALT), vec![0x1b, 0x0d]);
    }

    #[test]
    fn kitty_query_is_answered() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?u");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?0u",
            "must answer the kitty progressive-enhancement query so apps detect support"
        );
    }

    #[test]
    fn push_disambiguate_then_shift_enter_is_csi_u() {
        let mut term = Terminal::new(24, 80);
        // Exactly what a kitty-aware app sends to turn on the protocol.
        term.process(b"\x1b[>1u");
        assert!(
            term.keyboard_mode()
                .contains(KeyboardMode::DISAMBIGUATE_ESC_CODES),
            "push must enable disambiguate in the derived keyboard mode"
        );
        // Plain Enter still legacy CR; Shift+Enter becomes a distinct CSI-u report.
        assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
        assert_eq!(
            enc_enter(&term, Modifiers::SHIFT),
            b"\x1b[13;2u",
            "Shift+Enter under disambiguate must be ESC[13;2u — what Claude reads as a newline"
        );
        // And the query now reports the active flag.
        term.process(b"\x1b[?u");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1u");
    }

    #[test]
    fn codex_report_event_types_suppresses_predictive_echo() {
        let mut term = Terminal::new(24, 80);
        // Codex's observed main-screen composer mode: disambiguate + event types +
        // alternate keys, but notably not REPORT_ALL_KEYS_AS_ESC.
        term.process(b"\x1b[>7u");
        let mode = term.keyboard_mode();
        assert!(mode.contains(KeyboardMode::REPORT_EVENT_TYPES));
        assert!(!mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
        assert!(
            term.kitty_suppresses_predictive_echo(),
            "an app-owned Codex composer must never receive terminal ghost glyphs"
        );

        // Disambiguation alone is also used by line-echoing shells and must remain
        // eligible for conservative Adaptive prediction.
        term.process(b"\x1b[=1u");
        assert!(!term.kitty_suppresses_predictive_echo());
    }

    /// `kitty_reports_functional_keys` is true in exactly the modes where a key
    /// with no legacy form (the Menu key, `57363`) reaches the app, else host
    /// chrome steals a negotiated key or refuses one nobody receives. Pinned
    /// against the encoder, not a re-read of the flags.
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
        // Baseline: no negotiation ⇒ the Menu key encodes to nothing at all.
        let term = Terminal::new(24, 80);
        assert!(!term.kitty_reports_functional_keys());
        assert!(
            menu(&term).is_empty(),
            "legacy has no sequence for the Menu key"
        );

        // Disambiguate (`CSI > 1 u`) is the weakest flag that turns it on.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>1u");
        assert!(term.kitty_reports_functional_keys());
        assert_eq!(
            menu(&term),
            b"\x1b[57363u",
            "…and the app really does receive 57363"
        );

        // Report-all is the strongest and also reports it.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>8u");
        assert!(term.kitty_reports_functional_keys());
        assert!(term.kitty_report_all_keys());

        // Event types alone (`CSI > 2 u`) do not: a bare press still takes the
        // legacy path, so the app never sees the key and chrome may claim it.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>2u");
        assert!(!term.kitty_reports_functional_keys());
        assert!(
            menu(&term).is_empty(),
            "event-types-only still cannot deliver the Menu key"
        );
    }

    #[test]
    fn set_disambiguate_then_shift_enter_is_csi_u() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[=1u"); // CSI = 1 u (set, mode 1)
        assert_eq!(enc_enter(&term, Modifiers::SHIFT), b"\x1b[13;2u");
    }

    #[test]
    fn modify_other_keys_level2_distinguishes_shift_enter() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>4;2m"); // xterm modifyOtherKeys level 2
        assert_eq!(
            enc_enter(&term, Modifiers::SHIFT),
            b"\x1b[27;2;13~",
            "Shift+Enter under modifyOtherKeys L2 must be ESC[27;2;13~"
        );
    }

    #[test]
    fn push_flags_over_255_are_masked_not_saturated() {
        // kitty computes `val & 0x7f`: `CSI > 256 u` means flags 0, never
        // "saturate to 255 and enable every enhancement".
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>256u");
        term.process(b"\x1b[?u");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?0u",
            "undefined high bits must be masked out of the pushed flags"
        );
        // Same rule for CSI = flags u.
        term.process(b"\x1b[=257;1u"); // 257 & 0b1_1111 = 1
        term.process(b"\x1b[?u");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?1u");
    }

    #[test]
    fn decstr_resets_only_the_current_screens_kitty_state() {
        let mut term = Terminal::new(24, 80);
        // Negotiate on the main screen, then enter the alt screen (which has
        // its own flags/stack per the spec) and soft-reset there.
        term.process(b"\x1b[>1u");
        term.process(b"\x1b[?1049h"); // enter alt screen
        term.process(b"\x1b[>2u"); // alt-screen flags
        term.process(b"\x1b[!p"); // DECSTR
        term.process(b"\x1b[?u");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?0u",
            "DECSTR must clear the ACTIVE screen's kitty flags"
        );
        // Leaving the alt screen must restore the main screen's negotiation —
        // the soft reset happened on the alternate screen only.
        term.process(b"\x1b[?1049l");
        term.process(b"\x1b[?u");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?1u",
            "the inactive (main) screen's kitty state must survive an alt-screen DECSTR"
        );
    }

    // ConPTY win32-input-mode (DEC 9001). conhost sends `CSI ? 9001 h` at every
    // ConPTY start; without it a legacy Shift+Enter LF reaches PSReadLine as
    // Ctrl+Enter (InsertLineAbove), and Ctrl+Enter is indistinguishable from
    // Enter. These walk the negotiation and pin the bytes conhost receives.

    /// A session behind a console host: the posture aterm-gui applies to a
    /// ConPTY session (`spawn::apply_pty_keyboard_posture`).
    fn conpty_term() -> Terminal {
        let mut term = Terminal::new(24, 80);
        term.set_win32_input_enabled(true);
        term
    }

    /// The win32 key-record pair for `VK_RETURN`, spelled from the spec
    /// (`CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`), not from the encoder.
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
        // …and the lock-free word the GUI's input seam actually encodes from
        // carries it too (published at the end of the `process()` batch).
        assert!(
            term.mode_mirror()
                .keyboard_mode()
                .contains(KeyboardMode::WIN32_INPUT)
        );

        // Shift+Enter: SHIFT_PRESSED, UnicodeChar CR, the record a physical
        // Shift+Enter carries under Windows Terminal (`ReadKey` → Enter+Shift
        // CHAR=13, PSReadLine → AddLine), not aterm's Unix Shift+Enter LF policy.
        assert_eq!(
            enc_enter(&term, Modifiers::SHIFT),
            win32_enter_pair(13, 0x10)
        );
        // Ctrl+Enter / Ctrl+Shift+Enter: LEFT_CTRL_PRESSED, UnicodeChar LF —
        // Windows' own translation of a native Ctrl+Enter, Shift or not.
        assert_eq!(
            enc_enter(&term, Modifiers::CTRL),
            win32_enter_pair(10, 0x08)
        );
        assert_eq!(
            enc_enter(&term, Modifiers::CTRL | Modifiers::SHIFT),
            win32_enter_pair(10, 0x18)
        );
        // Plain Enter stays CR and Alt+Enter stays Meta-Enter: both work through
        // conhost's legacy translation.
        assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
        assert_eq!(enc_enter(&term, Modifiers::ALT), vec![0x1b, 0x0d]);

        // DECRST 9001 restores the legacy policy byte for byte.
        term.process(b"\x1b[?9001l");
        assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
        assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
        assert_eq!(enc_enter(&term, Modifiers::CTRL), vec![0x0d]);
    }

    #[test]
    fn conpty_9001_release_of_an_enter_chord_emits_no_second_key_up() {
        // The pair already carries its key-up; the GUI's Release event for the
        // same chord must therefore encode to nothing (the seam treats an empty
        // encoding as a faithful no-op).
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
        // The progressive-enhancement query still says "nothing negotiated".
        term.process(b"\x1b[?u");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?0u",
            "9001 is a host negotiation, not a kitty flag: the app must read ?0u"
        );
        // An app that then pushes kitty flags (conhost forwards the push) gets
        // its flags, but conhost is still the reader and drops a `CSI 13;2 u`
        // written back, so the chord stays a record; plain Enter is untouched.
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
        // Pop the app's flags: 9001 is still set — the pop never touched it,
        // because it never held it — and the record is unchanged.
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
        // conhost stops flushing a lone ESC once it has read one win32 record
        // (aterm's first Shift+Enter), after which Escape never reaches
        // `ReadKey` in that tab. Walk the negotiation and pin the bytes conhost
        // receives, through the lock-free word the GUI seam encodes from.
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
        // Ctrl+[ and Alt+[ dangle the same way and go the same way.
        assert_eq!(
            key(&term, Key::Character('['), Modifiers::CTRL),
            b"\x1b[219;26;27;1;8;1_\x1b[219;26;27;0;8;1_"
        );
        assert_eq!(
            key(&term, Key::Character('['), Modifiers::ALT),
            b"\x1b[219;26;91;1;2;1_\x1b[219;26;91;0;2;1_"
        );
        // Alt+a still reads as Alt+A after the switch: legacy bytes.
        assert_eq!(key(&term, Key::Character('a'), Modifiers::ALT), b"\x1ba");
        // DECRST 9001 restores the legacy ESC byte for byte.
        term.process(b"\x1b[?9001l");
        assert_eq!(key(&term, esc(), Modifiers::empty()), b"\x1b");
        assert_eq!(key(&term, Key::Character('['), Modifiers::ALT), b"\x1b[");
    }

    /// DEC 9001 is a negotiation with a Windows console host, and only a
    /// session a host put behind ConPTY has one. Anywhere else the request is
    /// program output (`cat` of a file holding `ESC [ ? 9001 h`), and it must
    /// change nothing: Escape, Ctrl+[ and the Enter chords keep their own
    /// bytes for whatever reads the tab next, and DECRQM answers as a terminal
    /// without the mode.
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
    fn conpty_9001_is_reported_by_decrqm_and_cleared_by_ris() {
        let mut term = conpty_term();
        term.process(b"\x1b[?9001$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?9001;2$y");
        term.process(b"\x1b[?9001h");
        term.process(b"\x1b[?9001$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?9001;1$y");
        // RIS: the mode resets with every other negotiated mode, so the next
        // conhost session starts from its own `CSI ? 9001 h`, not a stale one.
        term.process(b"\x1bc");
        assert!(!term.keyboard_mode().contains(KeyboardMode::WIN32_INPUT));
        assert_eq!(enc_enter(&term, Modifiers::SHIFT), vec![0x0a]);
        term.process(b"\x1b[?9001$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?9001;2$y");
    }

    /// The console-host posture is a function of the resize seam policy, both
    /// ways. Under `ConPty` conhost's 9001 takes and `CSI ? u` goes unanswered.
    /// Under `Native` the reverse holds even for an engine restored from a
    /// ConPTY checkpoint that carried the capability and the live mode.
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
}

#[cfg(test)]
mod kitty_capability_toggle_tests {
    //! `set_kitty_keyboard_enabled(false)` must make the terminal behave as if
    //! the kitty protocol is unsupported (the ConPTY embedder posture): no
    //! `CSI ? u` reply, push/set/pop consumed-and-ignored, no kitty bits in the
    //! derived keyboard mode, legacy key encoding throughout. Default-on
    //! behavior is locked by `shift_enter_e2e_tests` above.
    use super::shift_enter_e2e_tests::enc_enter;
    use crate::terminal::Terminal;
    use aterm_types::keyboard::{KeyboardMode, Modifiers};

    /// All kitty-derived bits of `KeyboardMode`.
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
        // Push, set, then pop — the full negotiation an app might attempt.
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
        // Nothing was forwarded to the grid as text: the sequences are consumed
        // like any unrecognized CSI (cursor still at origin, row empty).
        let cursor = term.cursor();
        assert_eq!((cursor.row, cursor.col), (0, 0), "sequence leaked to grid");
        // Encoding stays legacy: Shift+Enter is aterm's LF policy, not CSI-u.
        assert_eq!(enc_enter(&term, Modifiers::empty()), vec![0x0d]);
        assert_eq!(
            enc_enter(&term, Modifiers::SHIFT),
            vec![0x0a],
            "negotiation attempted while disabled must leave encoding legacy"
        );
        // And the query still gets no reply afterwards.
        term.process(b"\x1b[?u");
        assert_eq!(term.take_response(), None);
    }

    #[test]
    fn disabling_after_negotiation_drops_kitty_bits_and_reenabling_restores() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>1u"); // negotiated while supported
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
        // The toggle gates capability, it does not destroy negotiated state.
        term.set_kitty_keyboard_enabled(true);
        assert_eq!(enc_enter(&term, Modifiers::SHIFT), b"\x1b[13;2u");
    }

    #[test]
    fn disabled_survives_ris() {
        let mut term = Terminal::new(24, 80);
        term.set_kitty_keyboard_enabled(false);
        term.process(b"\x1bc"); // RIS: a program cannot re-enable the capability
        assert!(!term.is_kitty_keyboard_enabled());
        term.process(b"\x1b[?u");
        assert_eq!(term.take_response(), None);
    }
}
