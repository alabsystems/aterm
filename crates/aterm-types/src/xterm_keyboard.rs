// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).
//!
//! Extracted from `aterm-core::terminal::xterm_keyboard` to break circular
//! dependencies (Part of #5663, #2341).
//!
//! This module implements xterm's keyboard encoding mode control sequences:
//!
//! - **XTMODKEYS** (`CSI > Pp ; Pv m`): Controls modifier key encoding
//! - **XTFMTKEYS** (`CSI > Pp ; Pv f`): Controls key format options
//!
//! The most important parameter is `Pp=4` which controls `modifyOtherKeys`.
//! When enabled, keys with modifiers that wouldn't normally produce unique
//! codes are reported with CSI 27 ; modifier ; code ~ format.
//!
//! ## Precedence
//!
//! Kitty keyboard protocol takes precedence over xterm modifier encoding.
//! The FFI keyboard encoder checks Kitty flags first, then xterm state.

/// xterm keyboard modifier/format state.
///
/// Tracks the state of xterm's XTMODKEYS and XTFMTKEYS options.
/// Only `Pp=4` (modifyOtherKeys/formatOtherKeys) is currently supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XtermKeyboardState {
    /// modifyOtherKeys value (Pp=4 for XTMODKEYS).
    ///
    /// - `None`: Disabled (resource value -1, set via CSI > 4 n)
    /// - `Some(0)`: Default (no modification)
    /// - `Some(1)`: modifyOtherKeys level 1
    /// - `Some(2)`: modifyOtherKeys level 2
    modify_other_keys: Option<u8>,

    /// formatOtherKeys value (Pp=4 for XTFMTKEYS).
    ///
    /// - `0`: Default format (CSI 27 ; modifier ; code ~)
    /// - `1`: CSI code ; modifier u format (like Kitty)
    format_other_keys: u8,
}

impl XtermKeyboardState {
    /// Create a new default state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            modify_other_keys: Some(0),
            format_other_keys: 0,
        }
    }

    /// Get the modifyOtherKeys value.
    ///
    /// Returns `None` if disabled, or `Some(level)` where level is 0-2.
    #[must_use]
    pub const fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }

    /// Check if modifyOtherKeys is enabled (level > 0).
    #[must_use]
    pub const fn modify_other_keys_enabled(&self) -> bool {
        matches!(self.modify_other_keys, Some(v) if v > 0)
    }

    /// Get the formatOtherKeys value.
    #[must_use]
    pub const fn format_other_keys(&self) -> u8 {
        self.format_other_keys
    }

    /// Set modifyOtherKeys value (XTMODKEYS: CSI > 4 ; Pv m).
    ///
    /// Values are clamped to 0-2 per xterm spec.
    pub fn set_modify_other_keys(&mut self, value: u8) {
        self.modify_other_keys = Some(value.min(2));
    }

    /// Reset modifyOtherKeys to default (XTMODKEYS: CSI > 4 m).
    ///
    /// Sets the value to 0 (no modification) but keeps it enabled.
    pub fn reset_modify_other_keys(&mut self) {
        self.modify_other_keys = Some(0);
    }

    /// Disable modifyOtherKeys (XTMODKEYS: CSI > 4 n).
    ///
    /// Corresponds to xterm resource value -1.
    pub fn disable_modify_other_keys(&mut self) {
        self.modify_other_keys = None;
    }

    /// Set formatOtherKeys value (XTFMTKEYS: CSI > 4 ; Pv f).
    ///
    /// Values are clamped to 0-1 per xterm spec.
    pub fn set_format_other_keys(&mut self, value: u8) {
        self.format_other_keys = value.min(1);
    }

    /// Reset formatOtherKeys to default (XTFMTKEYS: CSI > 4 f).
    pub fn reset_format_other_keys(&mut self) {
        self.format_other_keys = 0;
    }

    /// Reset all xterm keyboard state to defaults.
    pub fn reset(&mut self) {
        self.modify_other_keys = Some(0);
        self.format_other_keys = 0;
    }

    /// Generate query response for modifyOtherKeys (CSI ? 4 m).
    ///
    /// Response format: CSI > 4 ; Pv m
    ///
    /// When disabled (`Pv = -1`), xterm reports `CSI > 4 m` (no value).
    #[must_use]
    pub fn query_modify_other_keys_response(self) -> String {
        match self.modify_other_keys {
            Some(v) => {
                // Trust gate: concatenation instead of `format!` —
                // runtime-argument `format_args!` cannot be lowered natively.
                // Byte-identical output.
                let mut resp = String::from("\x1b[>4;");
                resp.push_str(&v.to_string());
                resp.push('m');
                resp
            }
            None => "\x1b[>4m".to_string(),
        }
    }

    /// Generate query response for formatOtherKeys (CSI ? 4 g).
    ///
    /// Response format: CSI > 4 ; Pv f
    #[must_use]
    pub fn query_format_other_keys_response(self) -> String {
        // Trust gate: see `query_modify_other_keys_response`.
        let mut resp = String::from("\x1b[>4;");
        resp.push_str(&self.format_other_keys.to_string());
        resp.push('f');
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_state() {
        let state = XtermKeyboardState::new();
        assert_eq!(state.modify_other_keys(), Some(0));
        assert!(!state.modify_other_keys_enabled());
        assert_eq!(state.format_other_keys(), 0);
    }

    #[test]
    fn test_set_modify_other_keys() {
        let mut state = XtermKeyboardState::new();

        state.set_modify_other_keys(1);
        assert_eq!(state.modify_other_keys(), Some(1));
        assert!(state.modify_other_keys_enabled());

        state.set_modify_other_keys(2);
        assert_eq!(state.modify_other_keys(), Some(2));

        // Values > 2 are clamped
        state.set_modify_other_keys(10);
        assert_eq!(state.modify_other_keys(), Some(2));

        state.set_modify_other_keys(0);
        assert_eq!(state.modify_other_keys(), Some(0));
        assert!(!state.modify_other_keys_enabled());
    }

    #[test]
    fn test_reset_modify_other_keys() {
        let mut state = XtermKeyboardState::new();
        state.set_modify_other_keys(2);

        state.reset_modify_other_keys();
        assert_eq!(state.modify_other_keys(), Some(0));
        assert!(!state.modify_other_keys_enabled());
    }

    #[test]
    fn test_disable_modify_other_keys() {
        let mut state = XtermKeyboardState::new();
        state.set_modify_other_keys(2);

        state.disable_modify_other_keys();
        assert_eq!(state.modify_other_keys(), None);
        assert!(!state.modify_other_keys_enabled());
    }

    #[test]
    fn test_format_other_keys() {
        let mut state = XtermKeyboardState::new();

        state.set_format_other_keys(1);
        assert_eq!(state.format_other_keys(), 1);

        // Values > 1 are clamped
        state.set_format_other_keys(5);
        assert_eq!(state.format_other_keys(), 1);

        state.reset_format_other_keys();
        assert_eq!(state.format_other_keys(), 0);
    }

    #[test]
    fn test_query_responses() {
        let mut state = XtermKeyboardState::new();

        assert_eq!(state.query_modify_other_keys_response(), "\x1b[>4;0m");
        assert_eq!(state.query_format_other_keys_response(), "\x1b[>4;0f");

        state.set_modify_other_keys(2);
        state.set_format_other_keys(1);
        assert_eq!(state.query_modify_other_keys_response(), "\x1b[>4;2m");
        assert_eq!(state.query_format_other_keys_response(), "\x1b[>4;1f");
    }

    #[test]
    fn test_reset_all() {
        let mut state = XtermKeyboardState::new();
        state.set_modify_other_keys(2);
        state.set_format_other_keys(1);

        state.reset();
        assert_eq!(state.modify_other_keys(), Some(0));
        assert_eq!(state.format_other_keys(), 0);
    }
}
