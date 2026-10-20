// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform/modifier_supplement.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use crate::event::KeyEvent;
use crate::keyboard::Key;

/// Additional methods for the `KeyEvent` which cannot be implemented on all
/// platforms.
pub trait KeyEventExtModifierSupplement {
    /// Identical to `KeyEvent::text` but this is affected by <kbd>Ctrl</kbd>.
    ///
    /// For example, pressing <kbd>Ctrl</kbd>+<kbd>a</kbd> produces `Some("\x01")`.
    fn text_with_all_modifiers(&self) -> Option<&str>;

    /// This value ignores all modifiers including,
    /// but not limited to <kbd>Shift</kbd>, <kbd>Caps Lock</kbd>,
    /// and <kbd>Ctrl</kbd>. In most cases this means that the
    /// unicode character in the resulting string is lowercase.
    ///
    /// This is useful for key-bindings / shortcut key combinations.
    ///
    /// In case `logical_key` reports `Dead`, this will still report the
    /// key as `Character` according to the current keyboard layout. This value
    /// cannot be `Dead`.
    fn key_without_modifiers(&self) -> Key;

    /// [`KeyEvent::text`] at the press's <kbd>Shift</kbd> level with <kbd>Caps Lock</kbd>
    /// off: what a <kbd>Ctrl</kbd> or <kbd>Alt</kbd> chord names, which takes Shift's case
    /// and never the lock's.
    ///
    /// On X11 and Wayland xkb resolves Shift and Lock together, and an alphabetic key's type
    /// cancels one against the other: Turkish Caps Lock + Shift+i types `i`, where Shift
    /// alone types `İ`. So there the press is read again without the lock (ATERM-KI-1015-9).
    /// Elsewhere this is [`KeyEvent::text`].
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
