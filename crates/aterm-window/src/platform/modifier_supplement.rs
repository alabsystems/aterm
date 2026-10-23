// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/modifier_supplement.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use crate::event::KeyEvent;
use crate::keyboard::Key;

/// Additional methods for `KeyEvent` that cannot be implemented on all platforms.
pub trait KeyEventExtModifierSupplement {
    /// Identical to `KeyEvent::text` but affected by <kbd>Ctrl</kbd>.
    fn text_with_all_modifiers(&self) -> Option<&str>;

    /// Logical key ignoring all modifiers.
    fn key_without_modifiers(&self) -> Key;

    /// [`KeyEvent::text`] at the press's <kbd>Shift</kbd> level with <kbd>Caps Lock</kbd> off.
    fn text_ignoring_lock(&self) -> Option<&str>;
}

impl KeyEventExtModifierSupplement for KeyEvent {
    #[inline]
    fn text_with_all_modifiers(&self) -> Option<&str> {
        self.platform_specific.text_with_all_modifiers.as_ref().map(|s| s.as_str())
    }

    #[inline]
    fn key_without_modifiers(&self) -> Key {
        self.platform_specific.key_without_modifiers.clone()
    }

    #[inline]
    fn text_ignoring_lock(&self) -> Option<&str> {
        #[cfg(any(x11_platform, wayland_platform))]
        let text = &self.platform_specific.text_ignoring_lock;
        #[cfg(not(any(x11_platform, wayland_platform)))]
        let text = &self.text;
        text.as_deref()
    }
}
