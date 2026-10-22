// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cursor style types for terminal emulation.

/// Cursor style: the six DECSCUSR shapes plus frontend-only `Hidden` (DECTCEM off),
/// `HollowBlock` (outline, for unfocused windows and vi normal mode) and `Bolt`.
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CursorStyle {
    /// Blinking block (default) - Ps = 1.
    #[default]
    BlinkingBlock = 1,
    /// Steady block - Ps = 2.
    SteadyBlock = 2,
    /// Blinking underline - Ps = 3.
    BlinkingUnderline = 3,
    /// Steady underline - Ps = 4.
    SteadyUnderline = 4,
    /// Blinking bar - Ps = 5.
    BlinkingBar = 5,
    /// Steady bar - Ps = 6.
    SteadyBar = 6,
    /// Hidden cursor. Not a DECSCUSR parameter: DECTCEM off or the frontend's "no cursor".
    Hidden = 7,
    /// Hollow block cursor (outline only). Not a DECSCUSR parameter.
    HollowBlock = 8,
    /// Lightning-bolt cursor (a jagged silhouette filling the cell). Not a DECSCUSR parameter;
    /// the frontend's override while the `laser` cursor glow owns a block cursor. Steady, and
    /// paints over the glyph like the bar and underline shapes (no block cut-out).
    Bolt = 9,
}

impl CursorStyle {
    /// Map a DECSCUSR parameter to a cursor style.
    #[inline]
    #[must_use]
    pub const fn from_param(param: u16) -> Option<Self> {
        match param {
            0 | 1 => Some(Self::BlinkingBlock),
            2 => Some(Self::SteadyBlock),
            3 => Some(Self::BlinkingUnderline),
            4 => Some(Self::SteadyUnderline),
            5 => Some(Self::BlinkingBar),
            6 => Some(Self::SteadyBar),
            _ => None,
        }
    }

    /// Whether this style is one of the three blinking DECSCUSR shapes. The rendered blink
    /// follows the style, so this is the answer `CSI ? 12 $ p` (DEC mode 12) reports. The
    /// frontend-only shapes (`Hidden`, `HollowBlock`, `Bolt`) never blink.
    #[inline]
    #[must_use]
    pub const fn blinks(self) -> bool {
        matches!(
            self,
            Self::BlinkingBlock | Self::BlinkingUnderline | Self::BlinkingBar
        )
    }

    /// The same shape with its blink bit set to `blink`: DEC mode 12 (`CSI ? 12 h` blinking,
    /// `CSI ? 12 l` steady) never changes the shape. The frontend-only shapes pass through.
    #[inline]
    #[must_use]
    pub const fn with_blink(self, blink: bool) -> Self {
        match self {
            Self::BlinkingBlock | Self::SteadyBlock => {
                if blink {
                    Self::BlinkingBlock
                } else {
                    Self::SteadyBlock
                }
            }
            Self::BlinkingUnderline | Self::SteadyUnderline => {
                if blink {
                    Self::BlinkingUnderline
                } else {
                    Self::SteadyUnderline
                }
            }
            Self::BlinkingBar | Self::SteadyBar => {
                if blink {
                    Self::BlinkingBar
                } else {
                    Self::SteadyBar
                }
            }
            other => other,
        }
    }
}
