// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a release of a menu-bar status item's button means: the item's own act, or its menu.
//!
//! A status item that acts on a left click and opens its menu on a right click or a
//! Control-click has no `-menu` set (AppKit would open that on every press); its button sends
//! one action on both releases, and the handler reads the app's current event to tell them
//! apart. The mug (`aterm acaffeine`) and aterm's own `❯` item share this reading; it is pure,
//! so it is tested on every OS, and `aterm-macos-ui`'s `status_button` reads the event it is
//! given.

/// `NSEventTypeLeftMouseUp`.
pub const LEFT_MOUSE_UP: usize = 2;
/// `NSEventTypeRightMouseUp`.
pub const RIGHT_MOUSE_UP: usize = 4;
/// `NSEventModifierFlagControl`.
pub const CONTROL: usize = 1 << 18;

/// An event as [`classify`] tells one from another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventKey {
    /// `-[NSEvent type]`.
    pub kind: usize,
    /// `-[NSEvent modifierFlags]`.
    pub flags: usize,
    /// `-[NSEvent timestamp]`.
    pub at: f64,
    /// `-[NSEvent clickCount]` of a mouse button's event; 0 for any other.
    pub clicks: isize,
}

/// What a release of the button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// The item's own act: the mug toggles, the lock rotates.
    Primary,
    /// Open the menu.
    Menu,
}

/// The event a press is read from: `event` unless it is the one already read (`stale`: the
/// app's current event when the last press was read or the menu closed), which is no press of
/// its own. A press AppKit sends with no event of its own (an accessibility press) finds that
/// stale event still current.
fn fresh(event: Option<EventKey>, stale: Option<EventKey>) -> Option<EventKey> {
    event.filter(|event| Some(*event) != stale)
}

/// What a press of the button means when the app's current event is `event`: the menu for a
/// right release or a release with Control held, the item's own act for anything else, no
/// event and a `stale` one included.
#[must_use]
pub fn classify(event: Option<EventKey>, stale: Option<EventKey>) -> Click {
    match fresh(event, stale) {
        Some(event) if event.kind == RIGHT_MOUSE_UP || event.flags & CONTROL != 0 => Click::Menu,
        _ => Click::Primary,
    }
}

/// Whether the press is the second or a later click of a multi-click: a fresh left release
/// AppKit counts as 2 or more. An item whose act must not run twice on a double-click (the
/// lock, which would rotate two steps) takes such a press as none; the mug, which toggles on
/// every click, does not ask.
#[must_use]
pub fn repeated(event: Option<EventKey>, stale: Option<EventKey>) -> bool {
    fresh(event, stale).is_some_and(|event| event.kind == LEFT_MOUSE_UP && event.clicks >= 2)
}

/// Whether the press is a pointer's plain left release: a fresh left mouse-up with no Control
/// held. A press with no event of its own, one that finds a stale event (an accessibility
/// press) and a key's are not: an item whose act changes a security level (the lock) takes only
/// a pointed press as that act and opens its menu for every other, where the act is one pick.
#[must_use]
pub fn pointed(event: Option<EventKey>, stale: Option<EventKey>) -> bool {
    fresh(event, stale)
        .is_some_and(|event| event.kind == LEFT_MOUSE_UP && event.flags & CONTROL == 0)
}

#[cfg(test)]
#[path = "status_click_tests.rs"]
mod tests;
