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

/// Derive `KeyboardMode` from terminal modes, Kitty flags, and xterm state.
///
/// Delegates to `aterm_types::keyboard::TermMode::from_keyboard_state` as the
/// single source of truth for the 10-flag keyboard projection (#3732).
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
    // DECBKM (mode 67) is a legacy-encoding concern outside the TermMode kitty/xterm
    // projection, so fold it in here.
    if modes.backarrow_sends_bs {
        km.insert(KeyboardMode::BACKARROW_SENDS_BS);
    }
    // xterm keyboard private modes 1035/1036/1039 are likewise legacy-encoding
    // concerns folded in here. Each is modeled so the `empty()`/default mode
    // preserves the historical encoder contract:
    //   - 1039 altSendsEscape: a NEGATIVE flag — reset suppresses the Alt ESC.
    //   - 1036 metaSendsEscape: a POSITIVE flag — set adds the Meta ESC.
    //   - 1035 numLock: a NEGATIVE flag — reset strips the NumLock modifier.
    if !modes.alt_send_escape {
        km.insert(KeyboardMode::ALT_NO_ESC);
    }
    if modes.meta_send_escape {
        km.insert(KeyboardMode::META_SENDS_ESC);
    }
    if !modes.special_modifiers {
        km.insert(KeyboardMode::NO_SPECIAL_MODIFIERS);
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
    /// report, so the application by definition never receives echoing text.
    ///
    /// A NARROW read-only projection of [`Self::keyboard_mode`] for consumers that
    /// specifically need report-all semantics. Honors the same capability-off gating
    /// as `keyboard_mode()` (a disabled kitty capability reports `false`).
    #[must_use]
    pub fn kitty_report_all_keys(&self) -> bool {
        self.keyboard_mode()
            .contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }

    /// Whether the active Kitty keyboard mode makes predictive local echo unsafe.
    ///
    /// `REPORT_ALL_KEYS_AS_ESC` routes even plain text through CSI-u, so there is no
    /// ordinary line echo to predict. `REPORT_EVENT_TYPES` is the narrower but equally
    /// important application-owned-composer signal: full-screen-on-the-main-screen
    /// clients such as Codex request press/repeat/release reports while repainting their
    /// own input line. Painting terminal-level guesses there can briefly duplicate the
    /// client's text and then erase it at the predictor timeout.
    ///
    /// Keep this as a narrow, read-only projection so display-side callers do not need
    /// the full encoder-facing [`KeyboardMode`] word.
    #[must_use]
    pub fn kitty_suppresses_predictive_echo(&self) -> bool {
        self.keyboard_mode()
            .intersects(KeyboardMode::REPORT_EVENT_TYPES | KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
    }
}

#[cfg(test)]
mod shift_enter_e2e_tests {
    //! End-to-end ground truth for "Shift+Enter in Claude Code": drive the exact
    //! negotiation an app performs, then confirm the derived `KeyboardMode` and the
    //! bytes the encoder actually emits for Shift+Enter. This is the regression the
    //! keyboard test suites were missing entirely.
    use crate::terminal::Terminal;
    use aterm_types::keyboard::{
        Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout,
    };

    fn enc(term: &Terminal, mods: Modifiers) -> Vec<u8> {
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
        // aterm's input policy forces a usable Shift+Enter even with NO protocol and
        // NO faked identity: plain Enter is CR, Shift+Enter is LF (0x0a) — Claude /
        // readline / vim read LF as insert-newline. Ctrl+Enter stays CR; Alt+Enter is
        // Meta-Enter (ESC CR).
        assert_eq!(enc(&term, Modifiers::empty()), vec![0x0d]);
        assert_eq!(enc(&term, Modifiers::SHIFT), vec![0x0a]);
        assert_eq!(enc(&term, Modifiers::CTRL), vec![0x0d]);
        assert_eq!(enc(&term, Modifiers::ALT), vec![0x1b, 0x0d]);
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
        // Plain Enter still legacy CR; Shift+Enter becomes a DISTINCT CSI-u report.
        assert_eq!(enc(&term, Modifiers::empty()), vec![0x0d]);
        assert_eq!(
            enc(&term, Modifiers::SHIFT),
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
        // alternate keys, but notably NOT REPORT_ALL_KEYS_AS_ESC.
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

    #[test]
    fn set_disambiguate_then_shift_enter_is_csi_u() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[=1u"); // CSI = 1 u (set, mode 1)
        assert_eq!(enc(&term, Modifiers::SHIFT), b"\x1b[13;2u");
    }

    #[test]
    fn modify_other_keys_level2_distinguishes_shift_enter() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[>4;2m"); // xterm modifyOtherKeys level 2
        assert_eq!(
            enc(&term, Modifiers::SHIFT),
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
}

#[cfg(test)]
mod kitty_capability_toggle_tests {
    //! `set_kitty_keyboard_enabled(false)` must make the terminal behave as if
    //! the kitty protocol is UNSUPPORTED (the ConPTY embedder posture): no
    //! `CSI ? u` reply, push/set/pop consumed-and-ignored, no kitty bits in the
    //! derived keyboard mode, legacy key encoding throughout. Default-on
    //! behavior is locked by `shift_enter_e2e_tests` above.
    use crate::terminal::Terminal;
    use aterm_types::keyboard::{
        Key, KeyEventType, KeyboardMode, Modifiers, NamedKey, encode_key_with_layout,
    };

    /// All kitty-derived bits of `KeyboardMode`.
    const KITTY_BITS: KeyboardMode = KeyboardMode::DISAMBIGUATE_ESC_CODES
        .union(KeyboardMode::REPORT_EVENT_TYPES)
        .union(KeyboardMode::REPORT_ALTERNATE_KEYS)
        .union(KeyboardMode::REPORT_ALL_KEYS_AS_ESC)
        .union(KeyboardMode::REPORT_ASSOCIATED_TEXT);

    fn enc_enter(term: &Terminal, mods: Modifiers) -> Vec<u8> {
        encode_key_with_layout(
            &Key::Named(NamedKey::Enter),
            mods,
            term.keyboard_mode(),
            KeyEventType::Press,
            None,
        )
    }

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
