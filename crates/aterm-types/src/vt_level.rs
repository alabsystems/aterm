// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! VT conformance level tracking.
//!
//! Extracted from `aterm-core::vt_level` to `aterm-types` (Part of #5663).
//!
//! This module tracks which VT terminal level (VT100, VT220, VT320, VT420, VT520)
//! each escape sequence belongs to. This enables:
//!
//! - Proper DA1/DA2 (Device Attributes) response generation
//! - DECSCL (Set Conformance Level) handling
//! - Knowing which features require which terminal level
//!
//! ## VT Terminal Evolution
//!
//! | Level | Year | Key Features |
//! |-------|------|--------------|
//! | VT100 | 1978 | Basic ANSI escape sequences, 80/132 columns |
//! | VT220 | 1983 | User-defined keys, 8-bit controls, DRCS |
//! | VT320 | 1987 | 25th status line, locator (mouse) events |
//! | VT420 | 1990 | Rectangular area operations, macro recording |
//! | VT520 | 1995 | Session management, printer features |

/// VT terminal conformance level.
///
/// Each level implies support for all features from previous levels.
/// The integer values match the DA2 (Secondary Device Attributes) response.
///
/// # Not Orderable
///
/// `VtLevel` intentionally does NOT implement `PartialOrd`/`Ord`. DA2 parameter
/// values are assigned by DEC across decades with no ordering intent — e.g.,
/// VT330 (param 18) and VT340 (param 19) are supersets of VT320 (param 24).
/// Use capability methods (`supports_mouse()`, `supports_sixel()`, etc.) instead.
///
/// ```compile_fail
/// use aterm_types::VtLevel;
/// // VtLevel must not be comparable — DA2 params are not capability-ordered (#3883)
/// let _ = VtLevel::VT330 < VtLevel::VT320;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum VtLevel {
    /// VT100 (1978): Basic ANSI sequences, 80/132 columns, smooth scroll
    VT100 = 0,
    /// VT220 (1983): User-defined keys, 8-bit controls, DRCS soft fonts
    VT220 = 1,
    /// VT240 (1983): VT220 + ReGIS and Sixel graphics
    VT240 = 2,
    /// VT320 (1987): 25th status line, locator (mouse) input
    VT320 = 24,
    /// VT330 (1987): VT320 + monochrome Sixel graphics
    VT330 = 18,
    /// VT340 (1987): VT320 + color Sixel graphics
    VT340 = 19,
    /// VT420 (1990): Rectangular operations, macro recording, pages
    #[default]
    VT420 = 41,
    /// VT510 (1993): Enhanced character sets
    VT510 = 61,
    /// VT520 (1995): Session management, enhanced printing
    VT520 = 64,
    /// VT525 (1995): VT520 + color
    VT525 = 65,
}

impl VtLevel {
    /// Get the DA2 (Secondary Device Attributes) parameter for this level.
    ///
    /// This is the first parameter in the response to `CSI > c`.
    #[must_use]
    pub const fn da2_param(self) -> u8 {
        self as u8
    }

    /// Get the DECSCL (Set Conformance Level) parameter for this level.
    ///
    /// Used in `CSI Ps ; Ps " p` sequence.
    #[must_use]
    pub const fn decscl_param(self) -> u8 {
        match self {
            Self::VT100 => 61, // VT100 mode
            Self::VT220 | Self::VT240 => 62,
            Self::VT320 | Self::VT330 | Self::VT340 => 63,
            Self::VT420 => 64,
            Self::VT510 | Self::VT520 | Self::VT525 => 65,
        }
    }

    /// Create from DECSCL parameter value.
    #[must_use]
    pub const fn from_decscl_param(param: u8) -> Option<Self> {
        match param {
            61 => Some(Self::VT100),
            62 => Some(Self::VT220),
            63 => Some(Self::VT320),
            64 => Some(Self::VT420),
            65 => Some(Self::VT520),
            _ => None,
        }
    }

    /// Human-readable name of this terminal level.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::VT100 => "VT100",
            Self::VT220 => "VT220",
            Self::VT240 => "VT240",
            Self::VT320 => "VT320",
            Self::VT330 => "VT330",
            Self::VT340 => "VT340",
            Self::VT420 => "VT420",
            Self::VT510 => "VT510",
            Self::VT520 => "VT520",
            Self::VT525 => "VT525",
        }
    }
}

impl std::fmt::Display for VtLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decscl_param_roundtrip() {
        for level in [
            VtLevel::VT100,
            VtLevel::VT220,
            VtLevel::VT320,
            VtLevel::VT420,
            VtLevel::VT520,
        ] {
            let param = level.decscl_param();
            let recovered = VtLevel::from_decscl_param(param);
            assert_eq!(recovered, Some(level), "Roundtrip failed for {level}");
        }
    }
}
