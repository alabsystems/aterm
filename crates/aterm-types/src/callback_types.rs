// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callbacks the terminal uses to notify the UI layer of events (notifications, color
//! changes, clipboard and window operations).

use crate::{ClipboardOperation, Notification, Rgb, WindowOperation, WindowResponse};

/// Callback for a simple desktop notification (OSC 9), called with its message.
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

/// Callback for color changes (OSC 4, 10-21, 104, 110-112, 117, and 119), called with the
/// color that changed, its resulting value and the kind of change. The `u8` is:
/// - `0`: foreground (OSC 10, OSC 110 reset)
/// - `1`: background (OSC 11, OSC 111 reset)
/// - `2`: cursor (OSC 12, OSC 112 reset)
/// - `3`: indexed palette (OSC 4 / OSC 104)
/// - `4`: selection background (OSC 17 / OSC 21 / OSC 117)
/// - `5`: selection foreground (OSC 19 / OSC 21 / OSC 119)
pub type ColorChangeCallback = Box<dyn FnMut(u8, Rgb, ColorChangeOp) + Send>;

/// Callback for a complete kitty desktop notification (OSC 99).
pub type AdvancedNotificationCallback = Box<dyn FnMut(Notification) + Send>;

/// Callback for clipboard operations (OSC 52): returns the content for a query (`None` when
/// access is denied or unavailable); ignored for a set.
pub type ClipboardCallback = Box<dyn FnMut(ClipboardOperation) -> Option<String> + Send>;

/// Callback for window operations (CSI t, XTWINOPS): returns the response to a report
/// operation; ignored for manipulation operations (iconify, move, ...).
pub type WindowCallback = Box<dyn FnMut(WindowOperation) -> Option<WindowResponse> + Send>;
