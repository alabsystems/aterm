// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Core terminal types: capabilities and state snapshot.
//!
//! Extracted from `aterm-core::terminal::types::core` to break circular
//! dependencies and enable independent compilation (Part of #5663, #2341).

use std::sync::Arc;

use crate::CursorStyle;

// ============================================================================
// Terminal Capabilities
// ============================================================================

/// Terminal capabilities query result.
///
/// Reports what features this terminal emulator supports. Useful for
/// applications that want to query terminal capabilities before using
/// advanced features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "capability flags are naturally boolean"
)]
pub struct TerminalCapabilities {
    /// Terminal supports true color (24-bit RGB).
    pub true_color: bool,
    /// Terminal supports 256-color palette.
    pub color_256: bool,
    /// Terminal supports hyperlinks (OSC 8).
    pub hyperlinks: bool,
    /// Terminal supports Sixel graphics.
    pub sixel_graphics: bool,
    /// Terminal supports Terminal inline images.
    pub iterm_images: bool,
    /// Terminal supports Kitty graphics protocol.
    pub kitty_graphics: bool,
    /// Terminal supports clipboard operations (OSC 52).
    pub clipboard: bool,
    /// Terminal supports shell integration (OSC 133).
    pub shell_integration: bool,
    /// Terminal supports synchronized output (mode 2026).
    pub synchronized_output: bool,
    /// Terminal supports Kitty keyboard protocol.
    pub kitty_keyboard: bool,
    /// Terminal supports soft fonts (DRCS/DECDLD).
    pub soft_fonts: bool,
    /// Terminal supports unicode (always true for aterm).
    pub unicode: bool,
    /// Terminal supports bracketed paste mode.
    pub bracketed_paste: bool,
    /// Terminal supports focus reporting.
    pub focus_reporting: bool,
    /// Terminal supports mouse tracking.
    pub mouse_tracking: bool,
    /// Terminal supports alternate screen buffer.
    pub alternate_screen: bool,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self::aterm_capabilities()
    }
}

impl TerminalCapabilities {
    /// Get the full capabilities supported by aterm.
    #[must_use]
    pub const fn aterm_capabilities() -> Self {
        Self {
            true_color: true,
            color_256: true,
            hyperlinks: true,
            sixel_graphics: true,
            iterm_images: true,
            // Kitty graphics protocol: core transmit/display works (APC 'G' →
            // decode PNG/RGBA/RGB → place via the shared image pipeline), incl.
            // chunked transmission, the per-id store (a=t/a=p), targeted delete,
            // and the a=q support query. Advanced sub-features (o=z compression,
            // Unicode placeholders, animation, file/shared-mem mediums, z-index
            // compositing) degrade gracefully (skip, never garbage). Enforced by
            // `gate drift` (witness: handle_kitty_command). See KITTY-CORE in
            // docs/EXCEED_GHOSTTY_PLAN.md.
            kitty_graphics: true,
            clipboard: true,
            shell_integration: true,
            synchronized_output: true,
            kitty_keyboard: true,
            // Soft fonts (DRCS/DECDLD) are "permanently compiled out; consume
            // and ignore" in handler_dcs.rs — the DCS `{` payload is dropped.
            // Advertise false until a real DRCS implementation lands, so we do
            // not claim a capability that silently discards its data. Enforced
            // by `gate drift`. See docs/EXCEED_GHOSTTY_PLAN.md.
            soft_fonts: false,
            unicode: true,
            bracketed_paste: true,
            focus_reporting: true,
            mouse_tracking: true,
            alternate_screen: true,
        }
    }

    /// Format enabled capabilities as a semicolon-separated feature list.
    ///
    /// Used by OSC 62 feature reporting (xterm-401). Feature names follow
    /// xterm/kitty conventions where applicable.
    #[must_use]
    pub fn feature_list_string(&self) -> String {
        let mut features = Vec::with_capacity(16);
        if self.true_color {
            features.push("truecolor");
        }
        if self.color_256 {
            features.push("256color");
        }
        if self.hyperlinks {
            features.push("hyperlinks");
        }
        if self.sixel_graphics {
            features.push("sixel");
        }
        if self.iterm_images {
            features.push("Terminal-images");
        }
        if self.kitty_graphics {
            features.push("kitty-graphics");
        }
        if self.clipboard {
            features.push("clipboard");
        }
        if self.shell_integration {
            features.push("shell-integration");
        }
        if self.synchronized_output {
            features.push("sync-output");
        }
        if self.kitty_keyboard {
            features.push("kitty-keyboard");
        }
        if self.soft_fonts {
            features.push("soft-fonts");
        }
        if self.unicode {
            features.push("unicode");
        }
        if self.bracketed_paste {
            features.push("bracketed-paste");
        }
        if self.focus_reporting {
            features.push("focus-events");
        }
        if self.mouse_tracking {
            features.push("mouse");
        }
        if self.alternate_screen {
            features.push("altscreen");
        }
        features.join(";")
    }
}

// ============================================================================
// Terminal Snapshot
// ============================================================================

/// A snapshot of terminal state at a point in time.
///
/// This captures the essential state needed for diagnostics, debugging,
/// or state comparison without the full Terminal struct overhead.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "snapshot captures many terminal boolean states"
)]
pub struct TerminalSnapshot {
    /// Current cursor row (0-based).
    pub cursor_row: u16,
    /// Current cursor column (0-based).
    pub cursor_col: u16,
    /// Terminal width in columns.
    pub cols: u16,
    /// Terminal height in rows.
    pub rows: u16,
    /// Current window title.
    pub title: Arc<str>,
    /// Current working directory (if set).
    pub current_working_directory: Option<String>,
    /// Whether we're on the alternate screen.
    pub alternate_screen_active: bool,
    /// Whether origin mode is enabled.
    pub origin_mode: bool,
    /// Whether insert mode is enabled.
    pub insert_mode: bool,
    /// Whether cursor is visible.
    pub cursor_visible: bool,
    /// Current cursor style.
    pub cursor_style: CursorStyle,
    /// Total lines in scrollback (ring buffer + tiered).
    pub total_scrollback_lines: usize,
}

#[cfg(test)]
mod capability_tests {
    use super::TerminalCapabilities;

    #[test]
    fn kitty_graphics_advertised_after_kitty_core() {
        let feats = TerminalCapabilities::aterm_capabilities().feature_list_string();
        assert!(
            feats.contains("kitty-graphics"),
            "KITTY-CORE landed: kitty graphics must be advertised; got {feats:?}"
        );
    }

    #[test]
    fn soft_fonts_not_advertised() {
        // DECDLD is still compiled out, so soft-fonts stays honestly unadvertised.
        let feats = TerminalCapabilities::aterm_capabilities().feature_list_string();
        assert!(!feats.contains("soft-fonts"), "soft fonts unimplemented");
    }
}
