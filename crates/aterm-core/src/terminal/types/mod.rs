// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal type definitions.

pub use crate::grid::{CurrentStyle, SavedCursorState};

pub use aterm_types::TerminalModes;
pub use aterm_types::mouse::MouseEncoding;
pub use aterm_types::mouse::MouseMode;
pub use aterm_types::osc::Iterm2ShellIntegrationVersion;
pub use aterm_types::{ClipboardOperation, ClipboardSelection, CursorStyle, TerminalSize};

/// Bitmask of the SGR attribute groups a selective XTPUSHSGR (`CSI # { Ps…`) saved.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SgrPushMask(u16);

impl SgrPushMask {
    /// Push all attributes (no params or empty params).
    pub const ALL: Self = Self(0x3FF);

    const BOLD: u16 = 1 << 0;
    const UNDERLINE: u16 = 1 << 1;
    const BLINK: u16 = 1 << 2;
    const INVERSE: u16 = 1 << 3;
    const INVISIBLE: u16 = 1 << 4;
    const FOREGROUND: u16 = 1 << 5;
    const BACKGROUND: u16 = 1 << 6;
    const ITALIC: u16 = 1 << 7;
    const STRIKETHROUGH: u16 = 1 << 8;
    const OVERLINE: u16 = 1 << 9;

    /// Build a mask from XTPUSHSGR Ps parameter values.
    #[inline]
    pub const fn from_params(params: &[u16]) -> Self {
        let mut bits = 0u16;
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                1 | 2 => bits |= Self::BOLD, // dim is in the bold group per xterm
                3 => bits |= Self::ITALIC,
                4 | 21 => bits |= Self::UNDERLINE, // 21 = double underline
                5 => bits |= Self::BLINK,
                7 => bits |= Self::INVERSE,
                8 => bits |= Self::INVISIBLE,
                9 => bits |= Self::STRIKETHROUGH,
                30 | 31 | 38 | 39 => bits |= Self::FOREGROUND,
                40 | 41 | 48 | 49 => bits |= Self::BACKGROUND,
                53 => bits |= Self::OVERLINE,
                _ => {} // Unknown Ps — ignore per xterm
            }
            i += 1;
        }
        Self(bits)
    }

    /// The mask's bits, as a checkpoint carries them.
    #[inline]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// A mask from carried bits; a bit no attribute group names is dropped.
    #[inline]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & Self::ALL.0)
    }

    #[inline]
    pub const fn has_bold(self) -> bool {
        (self.0 & Self::BOLD) != 0
    }
    #[inline]
    pub const fn has_italic(self) -> bool {
        (self.0 & Self::ITALIC) != 0
    }
    #[inline]
    pub const fn has_underline(self) -> bool {
        (self.0 & Self::UNDERLINE) != 0
    }
    #[inline]
    pub const fn has_blink(self) -> bool {
        (self.0 & Self::BLINK) != 0
    }
    #[inline]
    pub const fn has_inverse(self) -> bool {
        (self.0 & Self::INVERSE) != 0
    }
    #[inline]
    pub const fn has_invisible(self) -> bool {
        (self.0 & Self::INVISIBLE) != 0
    }
    #[inline]
    pub const fn has_strikethrough(self) -> bool {
        (self.0 & Self::STRIKETHROUGH) != 0
    }
    #[inline]
    pub const fn has_foreground(self) -> bool {
        (self.0 & Self::FOREGROUND) != 0
    }
    #[inline]
    pub const fn has_background(self) -> bool {
        (self.0 & Self::BACKGROUND) != 0
    }
    #[inline]
    pub const fn has_overline(self) -> bool {
        (self.0 & Self::OVERLINE) != 0
    }
    #[inline]
    pub const fn is_all(self) -> bool {
        self.0 == Self::ALL.0
    }
}

/// SGR attribute stack entry for XTPUSHSGR/XTPOPSGR (`CSI # {` / `CSI # }`).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SgrStackEntry {
    /// Current style attributes (fg, bg, flags, protected).
    pub style: CurrentStyle,
    /// Underline color (SGR 58), raw: `0x01_RRGGBB` RGB or `0x02_0000NN` indexed.
    pub underline_color: Option<u32>,
    /// Which attributes were selectively pushed. `ALL` if no params given.
    pub mask: SgrPushMask,
}

pub(crate) use aterm_types::osc::RemoteHost;

pub(super) use aterm_types::TerminalCapabilities;
pub(super) use aterm_types::osc::{Notification, TaskbarProgress};
