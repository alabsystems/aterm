// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Clear methods for callbacks that previously lacked them.
//!
//! These correspond to `set_*_callback` methods in `callback_setters.rs`.
//! Without clear methods, the FFI bridge was forced to install no-op closures
//! (heap-allocating a `Box<dyn FnMut(...)>`) instead of setting the callback
//! to `None`. See #4185.
//!
//! `clear_advanced_notification_callback` lives in `callback_setters.rs`
//! alongside its setter.

use super::Terminal;

impl Terminal {
    /// Clear clipboard callback (OSC 52).
    pub fn clear_clipboard_callback(&mut self) {
        self.clipboard.callback = None;
    }

    /// Clear copy-to-clipboard callback (OSC 1337 CopyToClipboard).
    pub fn clear_copy_to_clipboard_callback(&mut self) {
        self.clipboard.copy_callback = None;
    }
}
