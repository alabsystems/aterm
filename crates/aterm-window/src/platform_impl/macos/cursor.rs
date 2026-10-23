// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm): Objective-C goes through `aterm_objc` (typed `send` prototypes,
// `Obj`/`Retained` handles, `MainThread` witnesses, the seam's `consts`) where upstream used objc2
// and its framework bindings. macOS has no custom cursors: this answers the system cursors.
use aterm_objc::send::{send_bool_id, send_id};
use aterm_objc::{autoreleasepool, class, sel, Obj};

use crate::window::CursorIcon;

/// Do two cursor handles denote the same cursor, by `-isEqual:`?
#[must_use]
pub(crate) fn same_cursor(a: &Obj, b: &Obj) -> bool {
    // SAFETY: both handles own +1 references to live objects; `-isEqual:` is `B@:@` on `NSObject`
    // and every subclass.
    unsafe { send_bool_id(a.id(), sel!(isEqual:), b.id()) }
}

pub(crate) fn default_cursor() -> Obj {
    class_cursor(sel!(arrowCursor))
}

/// A `+…Cursor` class accessor's result, retained.
#[must_use]
fn class_cursor(s: aterm_objc::Sel) -> Obj {
    // SAFETY: `s` is one of `NSCursor`'s `@16@0:8` class accessors (every call site passes a
    // literal one) and the result is +0 autoreleased. The pool is explicit: a caller may reach
    // `set_cursor` outside AppKit's own event pool.
    autoreleasepool(|_| {
        unsafe { Obj::retain(send_id(class(c"NSCursor").as_id(), s)) }
            .expect("+[NSCursor …Cursor] to answer a cursor")
    })
}

/// LOCAL PATCH (aterm): `NSCursor`'s system cursor for `icon`. aterm sets the first six on macOS;
/// the eight resize icons are its Wayland frame's, mapped here so every icon has an answer.
pub(crate) fn cursor_from_icon(icon: CursorIcon) -> Obj {
    match icon {
        CursorIcon::Default => default_cursor(),
        CursorIcon::Pointer => class_cursor(sel!(pointingHandCursor)),
        CursorIcon::Text => class_cursor(sel!(IBeamCursor)),
        CursorIcon::Grabbing => class_cursor(sel!(closedHandCursor)),
        CursorIcon::ColResize => class_cursor(sel!(resizeLeftRightCursor)),
        CursorIcon::RowResize => class_cursor(sel!(resizeUpDownCursor)),
        // LOCAL PATCH (aterm): the edges map as upstream's do; the corners are the arrow, because
        // upstream's corner cursors come from undocumented `+_windowResize…Cursor` accessors.
        CursorIcon::NResize => class_cursor(sel!(resizeUpCursor)),
        CursorIcon::SResize => class_cursor(sel!(resizeDownCursor)),
        CursorIcon::EResize => class_cursor(sel!(resizeRightCursor)),
        CursorIcon::WResize => class_cursor(sel!(resizeLeftCursor)),
        CursorIcon::NeResize
        | CursorIcon::SeResize
        | CursorIcon::SwResize
        | CursorIcon::NwResize => default_cursor(),
    }
}
