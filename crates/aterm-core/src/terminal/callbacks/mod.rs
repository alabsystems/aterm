// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callback types and infrastructure for terminal events.
//!
//! Callback type aliases live in the `aterm-types` crate.
//! This module re-exports them and keeps crate-internal sink adapters.

pub use aterm_types::ColorChangeOp;
use aterm_types::Rgb;

pub(super) use aterm_types::{
    AdvancedNotificationCallback, ClipboardCallback, NotificationCallback, WindowCallback,
};

// ----------------------------------------------------------------------------
// Crate-internal callback types (depend on aterm-core modules)
// ----------------------------------------------------------------------------

/// Target of a dynamic color change (OSC 10-19, palette). See
/// [`ColorChangeCallback`] for the callback signature.
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

/// Callback type for explicit, reset, and dynamic color changes (OSC 4, 10-21,
/// 104, 110-112, 117, and 119).
pub(super) type ColorChangeCallback = Box<dyn FnMut(ColorTarget, Rgb, ColorChangeOp) + Send>;

/// Callback type for dynamic color queries (OSC 10/11/12 with `?`): `Some`
/// overrides the palette color in the reply, `None` keeps it.
pub(super) type ColorQueryCallback = Box<dyn FnMut(ColorTarget) -> Option<Rgb> + Send>;

// ----------------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------------

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
