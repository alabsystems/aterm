// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).

/// xterm keyboard modifier/format state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XtermKeyboardState {
    /// modifyOtherKeys value (Pp=4 for XTMODKEYS).
    modify_other_keys: Option<u8>,

    /// formatOtherKeys value (Pp=4 for XTFMTKEYS).
    format_other_keys: u8,
}

impl XtermKeyboardState {
    /// Create a new default state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            modify_other_keys: Some(0),
            format_other_keys: 0,
        }
    }

    /// Get the modifyOtherKeys value.
    #[inline]
    #[must_use]
    pub const fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }

    /// Get the formatOtherKeys value.
    #[inline]
    #[must_use]
    pub const fn format_other_keys(&self) -> u8 {
        self.format_other_keys
    }

    /// Set modifyOtherKeys value (XTMODKEYS: CSI > 4 ; Pv m).
    #[inline]
    pub fn set_modify_other_keys(&mut self, value: u8) {
        self.modify_other_keys = Some(value.min(2));
    }

    /// Reset modifyOtherKeys to default (XTMODKEYS: CSI > 4 m).
    #[inline]
    pub const fn reset_modify_other_keys(&mut self) {
        self.modify_other_keys = Some(0);
    }

    /// Disable modifyOtherKeys (XTMODKEYS: CSI > 4 n).
    #[inline]
    pub const fn disable_modify_other_keys(&mut self) {
        self.modify_other_keys = None;
    }

    /// Set formatOtherKeys value (XTFMTKEYS: CSI > 4 ; Pv f).
    #[inline]
    pub fn set_format_other_keys(&mut self, value: u8) {
        self.format_other_keys = value.min(1);
    }

    /// Reset formatOtherKeys to default (XTFMTKEYS: CSI > 4 f).
    #[inline]
    pub const fn reset_format_other_keys(&mut self) {
        self.format_other_keys = 0;
    }

    /// Reset all xterm keyboard state to defaults.
    #[inline]
    pub const fn reset(&mut self) {
        self.modify_other_keys = Some(0);
        self.format_other_keys = 0;
    }

    /// Generate query response for modifyOtherKeys (CSI ? 4 m).
    #[must_use]
    pub fn query_modify_other_keys_response(self) -> String {
        match self.modify_other_keys {
            Some(v) => {
                let mut resp = String::from("\x1b[>4;");
                resp.push_str(&v.to_string());
                resp.push('m');
                resp
            }
            None => "\x1b[>4m".to_string(),
        }
    }

    /// Generate query response for formatOtherKeys (CSI ? 4 g).
    #[must_use]
    pub fn query_format_other_keys_response(self) -> String {
        let mut resp = String::from("\x1b[>4;");
        resp.push_str(&self.format_other_keys.to_string());
        resp.push('f');
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Default` (derive) is DISABLED (None) while `new()` is enabled at
    /// level 0, and the two answer the query differently: disabled reports no
    /// value parameter (`CSI > 4 m`), enabled an explicit 0.
    #[test]
    fn test_default_vs_new_divergence() {
        let default = XtermKeyboardState::default();
        let new = XtermKeyboardState::new();
        assert_eq!(default.modify_other_keys(), None);
        assert_eq!(new.modify_other_keys(), Some(0));
        assert_eq!(new.format_other_keys(), 0);
        assert_eq!(default.query_modify_other_keys_response(), "\x1b[>4m");
        assert_eq!(new.query_modify_other_keys_response(), "\x1b[>4;0m");
    }

    #[test]
    fn test_set_modify_other_keys() {
        let mut state = XtermKeyboardState::new();

        state.set_modify_other_keys(1);
        assert_eq!(state.modify_other_keys(), Some(1));
        assert!(matches!(state.modify_other_keys(), Some(v) if v > 0));

        state.set_modify_other_keys(2);
        assert_eq!(state.modify_other_keys(), Some(2));

        state.set_modify_other_keys(10);
        assert_eq!(state.modify_other_keys(), Some(2));

        state.set_modify_other_keys(0);
        assert_eq!(state.modify_other_keys(), Some(0));
        assert!(!matches!(state.modify_other_keys(), Some(v) if v > 0));
    }

    #[test]
    fn test_reset_modify_other_keys() {
        let mut state = XtermKeyboardState::new();
        state.set_modify_other_keys(2);

        state.reset_modify_other_keys();
        assert_eq!(state.modify_other_keys(), Some(0));
        assert!(!matches!(state.modify_other_keys(), Some(v) if v > 0));
    }

    #[test]
    fn test_disable_modify_other_keys() {
        let mut state = XtermKeyboardState::new();
        state.set_modify_other_keys(2);

        state.disable_modify_other_keys();
        assert_eq!(state.modify_other_keys(), None);
        assert!(!matches!(state.modify_other_keys(), Some(v) if v > 0));
    }

    #[test]
    fn test_format_other_keys() {
        let mut state = XtermKeyboardState::new();

        state.set_format_other_keys(1);
        assert_eq!(state.format_other_keys(), 1);

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
