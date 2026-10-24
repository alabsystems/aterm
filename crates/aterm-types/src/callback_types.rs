// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal callbacks for notifications, clipboard, and window operations.

use crate::{ClipboardOperation, Notification, WindowOperation, WindowResponse};

/// Callback for a simple desktop notification (OSC 9), called with its message.
pub type NotificationCallback = Box<dyn FnMut(&str) + Send>;

/// Callback for a complete kitty desktop notification (OSC 99).
pub type AdvancedNotificationCallback = Box<dyn FnMut(Notification) + Send>;

/// Callback for clipboard operations (OSC 52); returns query text or `None` when unavailable.
pub type ClipboardCallback = Box<dyn FnMut(ClipboardOperation) -> Option<String> + Send>;

/// Callback for window operations (CSI t, XTWINOPS); returns a report response when queried.
pub type WindowCallback = Box<dyn FnMut(WindowOperation) -> Option<WindowResponse> + Send>;
