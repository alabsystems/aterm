// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal type definitions.
//!
//! Public types (CursorStyle, TerminalSize, etc.) live in the `aterm-types`
//! crate. `CurrentStyle` and `SavedCursorState` live in `aterm-grid` (they
//! depend on grid primitives). This module re-exports all of them.

// ============================================================================
// Re-exports from aterm-grid (Part of #2341 checkpoint extraction prep)
// ============================================================================

pub use crate::grid::{CurrentStyle, SavedCursorState};

// ============================================================================
// Public re-exports from aterm-types (Part of #5663 Phase 1)
// ============================================================================

pub use aterm_types::TerminalModes;
pub use aterm_types::mouse::MouseEncoding;
pub use aterm_types::mouse::MouseMode;
pub use aterm_types::osc::{Iterm2CellSize, Iterm2SetColor, Iterm2ShellIntegrationVersion};
pub use aterm_types::{
    ClipboardOperation, ClipboardSelection, CopyToClipboardOperation, CursorStyle, TerminalSize,
    TerminalSnapshot,
};

// ============================================================================
// Terminal-only State Types
// ============================================================================

/// Bitmask for selective XTPUSHSGR attribute tracking.
///
/// Per xterm spec, `CSI # { Ps...` can selectively push only specific
/// SGR attributes. These flags track which attributes were saved.
///
/// Supported Ps values (per xterm ctlseqs):
/// 1/2=bold+dim, 3=italic, 4/21=underline, 5=blink, 7=inverse,
/// 8=invisible, 9=strikethrough, 30/31/38/39=foreground,
/// 40/41/48/49=background, 53=overline.
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
    pub fn from_params(params: &[u16]) -> Self {
        let mut bits = 0u16;
        for &p in params {
            match p {
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
        }
        Self(bits)
    }

    pub fn has_bold(self) -> bool {
        (self.0 & Self::BOLD) != 0
    }
    pub fn has_italic(self) -> bool {
        (self.0 & Self::ITALIC) != 0
    }
    pub fn has_underline(self) -> bool {
        (self.0 & Self::UNDERLINE) != 0
    }
    pub fn has_blink(self) -> bool {
        (self.0 & Self::BLINK) != 0
    }
    pub fn has_inverse(self) -> bool {
        (self.0 & Self::INVERSE) != 0
    }
    pub fn has_invisible(self) -> bool {
        (self.0 & Self::INVISIBLE) != 0
    }
    pub fn has_strikethrough(self) -> bool {
        (self.0 & Self::STRIKETHROUGH) != 0
    }
    pub fn has_foreground(self) -> bool {
        (self.0 & Self::FOREGROUND) != 0
    }
    pub fn has_background(self) -> bool {
        (self.0 & Self::BACKGROUND) != 0
    }
    pub fn has_overline(self) -> bool {
        (self.0 & Self::OVERLINE) != 0
    }
    pub fn is_all(self) -> bool {
        self.0 == Self::ALL.0
    }
}

/// SGR attribute stack entry for XTPUSHSGR/XTPOPSGR.
///
/// Stores all SGR-related state that can be pushed/popped via xterm's
/// SGR stack feature (CSI # { to push, CSI # } to pop).
///
/// When selective parameters are used (`CSI # { 1 ; 30`), only the
/// specified attributes are saved/restored. The `mask` field tracks
/// which attributes were pushed.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SgrStackEntry {
    /// Current style attributes (fg, bg, flags, protected).
    pub style: CurrentStyle,
    /// Underline color (SGR 58), stored as 0x01_RRGGBB or None.
    pub underline_color: Option<u32>,
    /// Which attributes were selectively pushed. `ALL` if no params given.
    pub mask: SgrPushMask,
}

// ============================================================================
// Crate-internal re-exports
// ============================================================================

pub(crate) use aterm_types::osc::{
    MultipartFileOperation, RemoteHost, SemanticBlock, SemanticBlockEvent, SemanticButton,
    SemanticButtonEvent, SemanticButtonType,
};

// ============================================================================
// Terminal-internal clipboard state
// ============================================================================

mod clipboard {
    use super::super::MAX_COPY_TO_CLIPBOARD_CAPTURE_BYTES;

    /// State for tracking CopyToClipboard text capture mode.
    ///
    /// When CopyToClipboard=name is received, the terminal enters text capture
    /// mode. All printed characters are accumulated until EndCopy is received.
    #[derive(Debug, Clone)]
    pub struct CopyToClipboardState {
        /// Named pasteboard to copy to.
        ///
        /// Read by the OSC 1337 CopyToClipboard executor in the FFI layer
        /// (ffi_bridge/); inert in the default lib build.
        #[allow(
            dead_code,
            reason = "copy target read by the OSC 1337 CopyToClipboard FFI executor"
        )]
        pub pasteboard: String,
        /// Accumulated text content.
        pub content: String,
        /// True once incoming content exceeds the capture cap.
        ///
        /// After overflow, we keep dropping all future chars so captured
        /// content remains a strict prefix of the stream.
        capture_truncated: bool,
    }

    impl CopyToClipboardState {
        /// Create a new capture state for the given pasteboard name.
        #[allow(
            dead_code,
            reason = "constructor consumed by the OSC 1337 CopyToClipboard FFI layer"
        )]
        pub fn new(pasteboard: String) -> Self {
            Self {
                pasteboard,
                content: String::new(),
                capture_truncated: false,
            }
        }

        /// Append a character to the captured content.
        pub fn push(&mut self, c: char) {
            if self.capture_truncated {
                return;
            }

            let char_len = c.len_utf8();
            if self.content.len().saturating_add(char_len) <= MAX_COPY_TO_CLIPBOARD_CAPTURE_BYTES {
                self.content.push(c);
            } else {
                self.capture_truncated = true;
            }
        }
    }
}

// ============================================================================
// Terminal-internal re-exports (only used within terminal/)
// ============================================================================

pub(super) use aterm_types::TerminalCapabilities;
pub(super) use aterm_types::osc::{Notification, TaskbarProgress, TextSizingOperation};
pub(super) use clipboard::CopyToClipboardState;
