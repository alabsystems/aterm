// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callback type definitions for terminal events.
//!
//! These type aliases define the function signatures for callbacks the Terminal
//! uses to notify the UI layer of various events (notifications, color
//! changes, clipboard and window operations).
//!
//! Extracted from `aterm-core::terminal::callbacks` (Part of #5663, Phase 2).

use crate::{ClipboardOperation, Notification, Rgb, WindowOperation, WindowResponse};

/// Callback type for simple desktop notifications (OSC 9).
///
/// Called when the terminal receives a simple notification escape sequence.
/// The UI layer should display a system notification with the message.
pub type NotificationCallback = Box<dyn FnMut(&str) + Send>;

/// Operation kind for dynamic color change callbacks.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChangeOp {
    /// The sequence set an explicit color value.
    Set,
    /// The sequence reset the color to its default value.
    Reset,
    /// The sequence selected a dynamic/automatic color rather than a fixed
    /// value (OSC 21 `key=`).
    Dynamic,
}

/// Callback type for color changes (OSC 4, 10-21, 104, 110-112, 117, and 119).
///
/// Called when a terminal color changes via escape sequences. The `u8`
/// parameter indicates which color changed:
/// - `0`: foreground (OSC 10, OSC 110 reset)
/// - `1`: background (OSC 11, OSC 111 reset)
/// - `2`: cursor (OSC 12, OSC 112 reset)
/// - `3`: indexed palette (OSC 4 / OSC 104)
/// - `4`: selection background (OSC 17 / OSC 21 / OSC 117)
/// - `5`: selection foreground (OSC 19 / OSC 21 / OSC 119)
///
/// The `Rgb` parameter is the resulting color value. `ColorChangeOp` tells the
/// consumer whether the sequence set an explicit value, reset to the configured
/// default, or selected dynamic/automatic behavior.
pub type ColorChangeCallback = Box<dyn FnMut(u8, Rgb, ColorChangeOp) + Send>;

/// Callback type for advanced desktop notifications (OSC 99 - kitty protocol).
///
/// Called when the terminal receives a complete notification via OSC 99.
/// The notification may include title, body, urgency, and an ID for updates.
pub type AdvancedNotificationCallback = Box<dyn FnMut(Notification) + Send>;

/// Callback type for clipboard operations (OSC 52).
///
/// The callback receives the clipboard operation and should return the clipboard
/// content for query operations (or None if clipboard access is denied/unavailable).
/// For set operations, the return value is ignored.
pub type ClipboardCallback = Box<dyn FnMut(ClipboardOperation) -> Option<String> + Send>;

/// Callback type for window operations (CSI t - XTWINOPS).
///
/// The callback receives the window operation and should return a response
/// for report operations. For manipulation operations (iconify, move, etc.),
/// the return value is ignored.
pub type WindowCallback = Box<dyn FnMut(WindowOperation) -> Option<WindowResponse> + Send>;
