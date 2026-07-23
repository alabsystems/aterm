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

    /// Create from DA2 parameter value.
    #[must_use]
    pub const fn from_da2_param(param: u8) -> Option<Self> {
        match param {
            0 => Some(Self::VT100),
            1 => Some(Self::VT220),
            2 => Some(Self::VT240),
            18 => Some(Self::VT330),
            19 => Some(Self::VT340),
            24 => Some(Self::VT320),
            41 => Some(Self::VT420),
            61 => Some(Self::VT510),
            64 => Some(Self::VT520),
            65 => Some(Self::VT525),
            _ => None,
        }
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

    /// Check if this level supports 8-bit C1 control codes.
    #[must_use]
    pub const fn supports_c1_controls(self) -> bool {
        matches!(
            self,
            Self::VT220
                | Self::VT240
                | Self::VT320
                | Self::VT330
                | Self::VT340
                | Self::VT420
                | Self::VT510
                | Self::VT520
                | Self::VT525
        )
    }

    /// Check if this level supports user-defined keys (DECUDK).
    #[must_use]
    pub const fn supports_user_defined_keys(self) -> bool {
        self.supports_c1_controls() // VT220+
    }

    /// Check if this level supports DRCS (downloadable soft fonts).
    #[must_use]
    pub const fn supports_drcs(self) -> bool {
        self.supports_c1_controls() // VT220+
    }

    /// Check if this level supports Sixel graphics.
    #[must_use]
    pub const fn supports_sixel(self) -> bool {
        matches!(self, Self::VT240 | Self::VT330 | Self::VT340 | Self::VT525)
    }

    /// Check if this level supports locator (mouse) input.
    #[must_use]
    pub const fn supports_mouse(self) -> bool {
        matches!(
            self,
            Self::VT320
                | Self::VT330
                | Self::VT340
                | Self::VT420
                | Self::VT510
                | Self::VT520
                | Self::VT525
        )
    }

    /// Check if this level supports rectangular area operations.
    #[must_use]
    pub const fn supports_rectangular_ops(self) -> bool {
        matches!(self, Self::VT420 | Self::VT510 | Self::VT520 | Self::VT525)
    }

    /// Check if this level supports multiple pages.
    #[must_use]
    pub const fn supports_pages(self) -> bool {
        matches!(self, Self::VT420 | Self::VT510 | Self::VT520 | Self::VT525)
    }

    /// Check if this level supports session management.
    #[must_use]
    pub const fn supports_sessions(self) -> bool {
        matches!(self, Self::VT520 | Self::VT525)
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

    /// Regression test for #3883: VtLevel capabilities work without ordering.
    ///
    /// The `compile_fail` doctest on the `VtLevel` type itself guards against
    /// re-adding `PartialOrd`/`Ord`. This test verifies the capability methods
    /// that replaced ordering-based comparisons.
    #[test]
    fn vt_level_no_derived_ordering() {
        // VT330/VT340 are supersets of VT320 (add Sixel graphics), but their
        // DA2 params are 18/19 vs VT320's 24. With derived Ord, VT330 < VT320.
        assert!(VtLevel::VT330.supports_mouse());
        assert!(VtLevel::VT340.supports_mouse());
        assert!(VtLevel::VT340.supports_sixel());
        assert!(!VtLevel::VT320.supports_sixel());
    }

    #[test]
    fn da2_param_roundtrip() {
        for level in [
            VtLevel::VT100,
            VtLevel::VT220,
            VtLevel::VT240,
            VtLevel::VT320,
            VtLevel::VT330,
            VtLevel::VT340,
            VtLevel::VT420,
            VtLevel::VT510,
            VtLevel::VT520,
            VtLevel::VT525,
        ] {
            let param = level.da2_param();
            let recovered = VtLevel::from_da2_param(param);
            assert_eq!(recovered, Some(level), "Failed for {level}");
        }
    }

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

    #[test]
    fn c1_controls_support() {
        assert!(!VtLevel::VT100.supports_c1_controls());
        assert!(VtLevel::VT220.supports_c1_controls());
        assert!(VtLevel::VT520.supports_c1_controls());
    }

    #[test]
    fn user_defined_keys_support() {
        assert!(!VtLevel::VT100.supports_user_defined_keys());
        assert!(VtLevel::VT220.supports_user_defined_keys());
        assert!(VtLevel::VT520.supports_user_defined_keys());
    }

    #[test]
    fn drcs_support() {
        assert!(!VtLevel::VT100.supports_drcs());
        assert!(VtLevel::VT220.supports_drcs());
        assert!(VtLevel::VT520.supports_drcs());
    }

    #[test]
    fn sixel_support() {
        assert!(!VtLevel::VT100.supports_sixel());
        assert!(!VtLevel::VT220.supports_sixel());
        assert!(VtLevel::VT240.supports_sixel());
        assert!(VtLevel::VT340.supports_sixel());
        assert!(!VtLevel::VT420.supports_sixel()); // VT420 doesn't have graphics
        assert!(VtLevel::VT525.supports_sixel());
    }

    #[test]
    fn mouse_support() {
        assert!(!VtLevel::VT100.supports_mouse());
        assert!(!VtLevel::VT220.supports_mouse());
        assert!(VtLevel::VT320.supports_mouse());
        assert!(VtLevel::VT520.supports_mouse());
    }

    #[test]
    fn rectangular_ops_support() {
        assert!(!VtLevel::VT100.supports_rectangular_ops());
        assert!(!VtLevel::VT320.supports_rectangular_ops());
        assert!(VtLevel::VT420.supports_rectangular_ops());
        assert!(VtLevel::VT520.supports_rectangular_ops());
    }

    #[test]
    fn pages_support() {
        assert!(!VtLevel::VT100.supports_pages());
        assert!(!VtLevel::VT320.supports_pages());
        assert!(VtLevel::VT420.supports_pages());
        assert!(VtLevel::VT520.supports_pages());
    }

    #[test]
    fn sessions_support() {
        assert!(!VtLevel::VT420.supports_sessions());
        assert!(VtLevel::VT520.supports_sessions());
        assert!(VtLevel::VT525.supports_sessions());
    }
}
