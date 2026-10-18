// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.
// (Every `objc2` binding call in this file was replaced with a typed send
// through `aterm_objc`; `Retained<NSCursor>` became `aterm_objc::Obj` in this
// file's `pub(crate)` return types. Search for the aterm local-patch marker.)

// LOCAL PATCH (aterm): objc2's `Retained`/`Sel` and its `NSCursor` bindings are
// gone. `Obj` is the +1 handle `Retained<NSCursor>` was.
//
// LOCAL PATCH (aterm), 2026-10-16 (docs/AUDIT-third-party-2026-10-15.md,
// winit-core step 5): custom cursors are gone on macOS (no aterm crate made
// one), and with them `CustomCursor`, its four hand-written traits and the
// `NSBitmapImageRep`/`NSImage`/`NSCursor` construction from RGBA bytes. What
// is left answers the six system cursors aterm sets.
use aterm_objc::send::{send_bool_id, send_id};
use aterm_objc::{autoreleasepool, class, sel, Obj};

use crate::window::CursorIcon;

/// Do two cursor handles denote the same cursor, by `-isEqual:`?
///
/// LOCAL PATCH (aterm): `window_delegate.rs`'s `set_cursor` compared two
/// `Retained<NSCursor>` with `==`, which objc2 forwarded here. Named rather
/// than re-derived at the call site.
#[must_use]
pub(crate) fn same_cursor(a: &Obj, b: &Obj) -> bool {
    // SAFETY: both handles own +1 references to live objects; `-isEqual:` is
    // `B@:@` on `NSObject` and every subclass.
    unsafe { send_bool_id(a.id(), sel!(isEqual:), b.id()) }
}

pub(crate) fn default_cursor() -> Obj {
    class_cursor(sel!(arrowCursor))
}

/// A `+…Cursor` class accessor's result, retained.
///
/// LOCAL PATCH (aterm): every accessor passed here is `@16@0:8` on `NSCursor`'s
/// metaclass and +0 autoreleased (none is named `new`/`alloc`/`copy`), so the retain
/// objc2's bindings applied is applied here once instead of at each call site.
#[must_use]
fn class_cursor(s: aterm_objc::Sel) -> Obj {
    // SAFETY: `s` is one of `NSCursor`'s `@16@0:8` class accessors (every call
    // site passes a literal one) and the result is +0 autoreleased. The pool is
    // explicit: a caller may reach `set_cursor` outside AppKit's own event pool.
    autoreleasepool(|_| {
        unsafe { Obj::retain(send_id(class(c"NSCursor").as_id(), s)) }
            .expect("+[NSCursor …Cursor] to answer a cursor")
    })
}

/// LOCAL PATCH (aterm), 2026-10-15 (winit-core step 5): six icons, the ones aterm sets
/// (`crate::cursor_icon`). Upstream's other 29 went with them: the undocumented `+_…Cursor`
/// accessors it reached through `-respondsToSelector:`/`-performSelector:`, and the WebKit
/// cursor PDFs it loaded from HIServices for `Move`, `AllScroll` and `Cell`.
pub(crate) fn cursor_from_icon(icon: CursorIcon) -> Obj {
    match icon {
        CursorIcon::Default => default_cursor(),
        CursorIcon::Pointer => class_cursor(sel!(pointingHandCursor)),
        CursorIcon::Text => class_cursor(sel!(IBeamCursor)),
        CursorIcon::Grabbing => class_cursor(sel!(closedHandCursor)),
        CursorIcon::ColResize => class_cursor(sel!(resizeLeftRightCursor)),
        CursorIcon::RowResize => class_cursor(sel!(resizeUpDownCursor)),
        // LOCAL PATCH (aterm), LW.5: the edge-resize icons, which aterm sets only on its own
        // Wayland frame. The four edges map as upstream's did; for the corners upstream reached
        // the undocumented `+_windowResize…Cursor` accessors, cut above, so they are the arrow.
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
