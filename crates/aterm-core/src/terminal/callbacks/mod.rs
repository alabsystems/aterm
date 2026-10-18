// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callback types for terminal events: the `aterm-types` aliases re-exported, the
//! color-change callbacks, and the protocol stack and buffer limits.

pub(super) use aterm_types::{
    AdvancedNotificationCallback, ClipboardCallback, NotificationCallback, WindowCallback,
};

/// Target of a dynamic color change (OSC 10-19, palette).
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
    /// Selection foreground color (OSC 19 / OSC 21).
    SelectionForeground,
}

/// Most bytes all active DCS operations may hold (`dcs.total_bytes`); past it
/// new DCS data is dropped until existing operations complete. Enforced in
/// `handler_dcs.rs` and `handler_actions.rs`.
pub(super) const MAX_DCS_GLOBAL_BUDGET: usize = 10 * 1024 * 1024;

/// Maximum depth of the title stack, so hostile pushes cannot grow memory
/// without bound (likewise the two stacks below).
pub(super) const TITLE_STACK_MAX_DEPTH: usize = 10;

/// Maximum depth of the SGR attribute stack (XTPUSHSGR/XTPOPSGR), as in xterm.
pub(super) const SGR_STACK_MAX_DEPTH: usize = 10;

/// Maximum depth of the Kitty color stack (OSC 30001/30101): the protocol's
/// minimum of 16.
pub(super) const COLOR_STACK_MAX_DEPTH: usize = 16;
