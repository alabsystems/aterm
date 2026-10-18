// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Callbacks the terminal uses to notify the UI layer of events (notifications, clipboard
//! and window operations).

use crate::{ClipboardOperation, Notification, WindowOperation, WindowResponse};

/// Callback for a simple desktop notification (OSC 9), called with its message.
pub type NotificationCallback = Box<dyn FnMut(&str) + Send>;

/// Callback for a complete kitty desktop notification (OSC 99).
pub type AdvancedNotificationCallback = Box<dyn FnMut(Notification) + Send>;

/// Callback for clipboard operations (OSC 52): returns the content for a query (`None` when
/// access is denied or unavailable); ignored for a set.
pub type ClipboardCallback = Box<dyn FnMut(ClipboardOperation) -> Option<String> + Send>;

/// Callback for window operations (CSI t, XTWINOPS): returns the response to a report
/// operation; ignored for manipulation operations (iconify, move, ...).
pub type WindowCallback = Box<dyn FnMut(WindowOperation) -> Option<WindowResponse> + Send>;
