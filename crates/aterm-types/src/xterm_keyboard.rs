// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! xterm keyboard modifier/format options (XTMODKEYS/XTFMTKEYS).

/// xterm keyboard modifier/format state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct XtermKeyboardState {
    /// modifyOtherKeys value (Pp=4 for XTMODKEYS).
    #[cfg_attr(
        feature = "serde",
        serde(deserialize_with = "deserialize_modify_other_keys")
    )]
    modify_other_keys: Option<u8>,

    /// formatOtherKeys value (Pp=4 for XTFMTKEYS).
    #[cfg_attr(
        feature = "serde",
        serde(deserialize_with = "deserialize_format_other_keys")
    )]
    format_other_keys: u8,
}

#[cfg(feature = "serde")]
fn deserialize_modify_other_keys<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<u8>, D::Error> {
    Ok(<Option<u8> as serde::Deserialize>::deserialize(d)?.map(|v| v.min(2)))
}

#[cfg(feature = "serde")]
fn deserialize_format_other_keys<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    Ok(<u8 as serde::Deserialize>::deserialize(d)?.min(1))
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

impl Default for XtermKeyboardState {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "xterm_keyboard_tests.rs"]
mod tests;
