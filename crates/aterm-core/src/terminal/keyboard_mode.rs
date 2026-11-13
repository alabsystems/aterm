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
#[path = "keyboard_mode_shift_enter_e2e_tests.rs"]
mod shift_enter_e2e_tests;

#[cfg(test)]
#[path = "keyboard_mode_kitty_capability_toggle_tests.rs"]
mod kitty_capability_toggle_tests;

#[cfg(test)]
#[path = "keyboard_mode_console_host_modify_other_keys_tests.rs"]
mod console_host_modify_other_keys_tests;

#[cfg(test)]
#[path = "keyboard_mode_control_chord_tests.rs"]
mod control_chord_tests;
