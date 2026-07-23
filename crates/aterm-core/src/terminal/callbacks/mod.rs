// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callback types and infrastructure for terminal events.
//!
//! Callback type aliases and registry metadata live in `aterm-types` crate.
//! This module re-exports them, normalizes protocol events for callback
//! consumers, and keeps crate-internal sink adapters (Part of #5663 Phase 2).

pub use aterm_types::ColorChangeOp;
use aterm_types::Rgb;
pub use aterm_types::callback_events::{SshConductorCallbackEvent, TmuxCallbackEvent};
pub use aterm_types::{
    CALLBACK_REGISTRY, CallbackCategory, CallbackInfo, callback_by_name, callback_count,
    callback_info,
};

// ----------------------------------------------------------------------------
// Re-export callback type aliases from aterm-types (Part of #5663 Phase 2)
// ----------------------------------------------------------------------------

pub(super) use aterm_types::{
    AdvancedNotificationCallback, BufferActivationCallback, ClipboardCallback,
    CopyToClipboardCallback, DcsCallback, HighlightCursorLineCallback, KvpCallback,
    NotificationCallback, RemoteHostCallback, ReportCellSizeCallback, ReportVariableCallback,
    SemanticBlockCallback, SemanticButtonCallback, SetBadgeFormatCallback, SetColorsCallback,
    SetProfileCallback, ShellIntegrationVersionCallback, TextSizingCallback, TitleCallback,
    TitleEventCallback, WindowCallback,
};

// ----------------------------------------------------------------------------
// Crate-internal callback types (depend on aterm-core modules)
// ----------------------------------------------------------------------------

/// Target of a dynamic color change (OSC 10-19, palette).
///
/// Replaces the magic `u8` indices previously used in color change callbacks.
/// See [`ColorChangeCallback`] for callback signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColorTarget {
    /// Default foreground color (OSC 10).
    Foreground,
    /// Default background color (OSC 11).
    Background,
    /// Cursor color (OSC 12).
    Cursor,
    /// Palette color changed (OSC 4, OSC 104).
    Palette,
    /// Selection background color (OSC 17 / OSC 21).
    SelectionBackground,
    /// Selection foreground color (OSC 19).
    SelectionForeground,
}

impl ColorTarget {
    /// Convert from legacy `u8` index (0=fg, 1=bg, 2=cursor, 3=palette, 4=sel_bg, 5=sel_fg).
    #[must_use]
    pub fn from_u8(index: u8) -> Option<Self> {
        Some(match index {
            0 => Self::Foreground,
            1 => Self::Background,
            2 => Self::Cursor,
            3 => Self::Palette,
            4 => Self::SelectionBackground,
            5 => Self::SelectionForeground,
            _ => return None,
        })
    }

    /// Convert to legacy `u8` index for FFI compatibility.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Foreground => 0,
            Self::Background => 1,
            Self::Cursor => 2,
            Self::Palette => 3,
            Self::SelectionBackground => 4,
            Self::SelectionForeground => 5,
        }
    }
}

/// Callback type for dynamic color changes (OSC 10/11/12, OSC 110/111/112).
pub(super) type ColorChangeCallback = Box<dyn FnMut(ColorTarget, Rgb, ColorChangeOp) + Send>;

/// Callback type for dynamic color queries (OSC 10/11/12 with `?`).
///
/// Called when an application queries the terminal's foreground, background, or
/// cursor color. The callback receives the [`ColorTarget`] and may return an
/// override `Rgb` value to use in the response instead of the terminal's
/// internal palette. Returning `None` uses the palette color.
pub(super) type ColorQueryCallback = Box<dyn FnMut(ColorTarget) -> Option<Rgb> + Send>;

// ----------------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------------

/// Maximum bytes per DCS callback invocation.
pub(super) const MAX_DCS_CALLBACK_BYTES: usize = 1_048_576;

/// Global maximum DCS memory budget (10 MB).
///
/// This limits total memory used by all active DCS operations across the terminal.
/// If exceeded, new DCS data is silently dropped until existing operations complete.
/// Enforced in handler.rs via dcs_total_bytes tracking.
pub(super) const MAX_DCS_GLOBAL_BUDGET: usize = 10 * 1024 * 1024;

/// Maximum depth of the title stack.
///
/// Prevents unbounded memory growth from malicious sequences.
pub(super) const TITLE_STACK_MAX_DEPTH: usize = 10;

/// Maximum depth of the SGR attribute stack (XTPUSHSGR/XTPOPSGR).
///
/// Prevents unbounded memory growth from malicious sequences.
/// Same depth as xterm's implementation.
pub(super) const SGR_STACK_MAX_DEPTH: usize = 10;

/// Maximum depth of the color stack (OSC 30001/30101 - Kitty protocol).
///
/// Prevents unbounded memory growth from malicious sequences.
/// Kitty specifies minimum 16 entries; we use exactly 16.
pub(super) const COLOR_STACK_MAX_DEPTH: usize = 16;
