// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Census of the selectors `vendor/winit` sends, beside `winit_seam.rs`'s
//! census of the methods it declares.
//!
//! A hand-written send has no binding crate to supply the SDK's argument
//! types, and a wrong one can be invisible at run time (e.g.
//! `-requestUserAttention:` is `q24@0:8Q16`: signed return, unsigned
//! argument). So each row names the `send_*` helper the fork calls, the
//! expected encoding is derived from the helper's name, and it is compared with
//! the runtime's own encoding.
//!
//! Non-vacuity: without AppKit [`load_appkit`] returns false and the test
//! panics; an unresolved selector fails unless listed in
//! [`NO_STATIC_ENCODING`] with a reason; [`shape_of`] panics on an unknown
//! helper name, and [`the_expected_shapes_are_not_all_the_same`] requires more
//! than one shape.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString};

use aterm_objc::{class, sel_uncached, strip_method_offsets};

use crate::appkit;
use crate::appkit::load_appkit;
use crate::winit_fork::{fork_rs_files, fork_sources};

/// [`appkit::shape_of`] over this census's vocabulary, [`token`].
fn shape_of(helper: &str) -> String {
    appkit::shape_of(helper, token)
}

/// One name from a helper's `send_a_b_c` spelling, as its encoding.
fn token(t: &str) -> &'static str {
    match t {
        "v" => "v",
        "id" => "@",
        // `BOOL` differs by arch (`B` on aarch64, `c` on x86_64), so use the
        // crate's own arch-dependent `Bool::ENCODING`.
        "bool" => <aterm_objc::Bool as aterm_objc::Encode>::ENCODING,
        "isize" => "q",
        "usize" => "Q",
        "f64" => "d",
        // The C `float`: `NSEvent`'s `-rotation`/`-pressure` are `f` while
        // `-magnification` is `d`; on AAPCS64 `f` returns in `s0`, so the wrong
        // prototype reads garbage.
        "f32" => "f",
        "u16" => "S",
        // The C `unsigned int` (`-[NSNumber unsignedIntValue]`, for a
        // `CGDirectDisplayID`); AAPCS64 leaves the upper 32 bits of `x0`
        // unspecified for an `int` return.
        "u32" => "I",
        // Signed short: `NSEventSubtype` in `+otherEventWithType:…subtype:`
        // (`s`, not `S`).
        "i16" => "s",
        "sel" => ":",
        "cls" => "#",
        // `Protocol *` encodes `@` (an object), not `#`.
        "proto" => "@",
        // `ptr` is a bare `void *` returned untouched (KVO's `context:`); `idptr`
        // is a const pointer to objects Foundation reads and retains
        // (`+arrayWithObjects:count:`). They encode differently.
        "ptr" => "^v",
        "idptr" => "r^@",
        // The remaining pointer tokens; each is a distinct encoding:
        //
        //   ptr      ^v    KVO's `context:` — opaque, never dereferenced
        //   idptr    r^@   `+arrayWithObjects:count:` — const, retained
        //   cptr     r^v   `-initWithBytes:length:` — const, copied
        //   charptr  *     `-bitmapData` — an interior `unsigned char *`
        //   planeptr ^*    `-initWithBitmapDataPlanes:…` — an ARRAY of those
        //
        // clang encodes every `char *` flavour as a bare `*`, never `^C`.
        "cptr" => "r^v",
        "charptr" => "*",
        "planeptr" => "^*",
        "rect" => "{CGRect={CGPoint=dd}{CGSize=dd}}",
        "point" => "{CGPoint=dd}",
        "size" => "{CGSize=dd}",
        "range" => "{_NSRange=QQ}",
        other => panic!(
            "unknown helper token {other:?} — add it to `token` rather than \
             letting the census accept a shape it cannot spell"
        ),
    }
}

/// Rows whose encoding the runtime cannot be asked for STATICALLY, with the
/// reason. Each is still exercised for real by the drivers.
const NO_STATIC_ENCODING: &[(&str, &str, &str)] = &[
    // `selectedWindow` is a dynamic property (`T@"NSWindow",W,D`), so
    // `class_getInstanceMethod` answers NULL until AppKit resolves it. An object
    // property setter is `v@:@` on every ABI.
    ("NSWindowTabGroup", "setSelectedWindow:", "send_v_id"),
    // `-draggingPasteboard` belongs to the `NSDraggingInfo` PROTOCOL, not to a
    // class, so it is checked against the protocol below instead.
    ("NSObject", "draggingPasteboard", "send_id"),
    // `NSMutableAttributedString` is a class cluster: neither it nor
    // `NSAttributedString` implements these initialisers; the IMP lives on a
    // private concrete subclass `+alloc` substitutes
    // (`NSConcreteMutableAttributedString`). Both are `@@:@`, and
    // `objc_ime_drive` exercises them.
    ("NSMutableAttributedString", "initWithString:", "send_id_id"),
    (
        "NSMutableAttributedString",
        "initWithAttributedString:",
        "send_id_id",
    ),
];

/// Every selector `window_delegate.rs` sends, its receiver's class, and the
/// helper the fork calls for it.
const ROWS: &[(&str, &str, &str)] = &[
    // ---- NSWindow ----
    ("NSWindow", "setDelegate:", "send_v_id"),
    ("NSWindow", "occlusionState", "send_usize"),
    ("NSWindow", "screen", "send_id"),
    ("NSWindow", "setFrame:display:", "send_v_rect_bool"),
    ("NSWindow", "frame", "send_rect"),
    ("NSWindow", "setReleasedWhenClosed:", "send_v_bool"),
    ("NSWindow", "setTitle:", "send_v_id"),
    ("NSWindow", "setAcceptsMouseMovedEvents:", "send_v_bool"),
    ("NSWindow", "setTabbingIdentifier:", "send_v_id"),
    ("NSWindow", "setTabbingMode:", "send_v_isize"),
    ("NSWindow", "setSharingType:", "send_v_usize"),
    ("NSWindow", "setTitlebarAppearsTransparent:", "send_v_bool"),
    ("NSWindow", "setTitleVisibility:", "send_v_isize"),
    ("NSWindow", "standardWindowButton:", "send_id_usize"),
    ("NSWindow", "setMovableByWindowBackground:", "send_v_bool"),
    ("NSWindow", "setHasShadow:", "send_v_bool"),
    ("NSWindow", "center", "send_v"),
    ("NSWindow", "setContentView:", "send_v_id"),
    ("NSWindow", "setInitialFirstResponder:", "send_v_id"),
    ("NSWindow", "setOpaque:", "send_v_bool"),
    ("NSWindow", "setBackgroundColor:", "send_v_id"),
    ("NSWindow", "registerForDraggedTypes:", "send_v_id"),
    ("NSWindow", "addChildWindow:ordered:", "send_v_id_isize"),
    ("NSWindow", "backingScaleFactor", "send_f64"),
    ("NSWindow", "setAppearance:", "send_v_id"),
    (
        "NSWindow",
        "addObserver:forKeyPath:options:context:",
        "send_v_id_id_usize_ptr",
    ),
    ("NSWindow", "removeObserver:forKeyPath:", "send_v_id_id"),
    ("NSWindow", "contentView", "send_id"),
    ("NSWindow", "contentRectForFrameRect:", "send_rect_rect"),
    ("NSWindow", "setContentSize:", "send_v_size"),
    ("NSWindow", "setStyleMask:", "send_v_usize"),
    ("NSWindow", "makeFirstResponder:", "send_bool_id"),
    ("NSWindow", "windowNumber", "send_isize"),
    ("NSWindow", "makeKeyAndOrderFront:", "send_v_id"),
    ("NSWindow", "orderOut:", "send_v_id"),
    ("NSWindow", "orderFront:", "send_v_id"),
    ("NSWindow", "isVisible", "send_bool"),
    ("NSWindow", "setFrameOrigin:", "send_v_point"),
    ("NSWindow", "setContentMinSize:", "send_v_size"),
    ("NSWindow", "setContentMaxSize:", "send_v_size"),
    ("NSWindow", "setContentResizeIncrements:", "send_v_size"),
    ("NSWindow", "styleMask", "send_usize"),
    ("NSWindow", "isResizable", "send_bool"),
    ("NSWindow", "isMiniaturizable", "send_bool"),
    ("NSWindow", "hasCloseBox", "send_bool"),
    ("NSWindow", "invalidateCursorRectsForView:", "send_v_id"),
    ("NSWindow", "performWindowDragWithEvent:", "send_v_id"),
    ("NSWindow", "setIgnoresMouseEvents:", "send_v_bool"),
    ("NSWindow", "isZoomed", "send_bool"),
    ("NSWindow", "isMiniaturized", "send_bool"),
    ("NSWindow", "miniaturize:", "send_v_id"),
    ("NSWindow", "deminiaturize:", "send_v_id"),
    ("NSWindow", "zoom:", "send_v_id"),
    ("NSWindow", "setLevel:", "send_v_isize"),
    ("NSWindow", "toggleFullScreen:", "send_v_id"),
    ("NSWindow", "isKeyWindow", "send_bool"),
    ("NSWindow", "appearance", "send_id"),
    ("NSWindow", "setMovable:", "send_v_bool"),
    ("NSWindow", "hasShadow", "send_bool"),
    ("NSWindow", "tabbingIdentifier", "send_id"),
    ("NSWindow", "selectNextTab:", "send_v_id"),
    ("NSWindow", "selectPreviousTab:", "send_v_id"),
    ("NSWindow", "tabGroup", "send_id"),
    ("NSWindow", "tabbedWindows", "send_id"),
    ("NSWindow", "isDocumentEdited", "send_bool"),
    ("NSWindow", "setDocumentEdited:", "send_v_bool"),
    ("NSWindow", "title", "send_id"),
    (
        "NSWindow",
        "performSelector:withObject:afterDelay:",
        "send_v_sel_id_f64",
    ),
    // ---- NSApplication ----
    ("NSApplication", "presentationOptions", "send_usize"),
    ("NSApplication", "setPresentationOptions:", "send_v_usize"),
    ("NSApplication", "currentEvent", "send_id"),
    ("NSApplication", "activateIgnoringOtherApps:", "send_v_bool"),
    // Signed return over an unsigned argument: `q` out, `Q` in.
    ("NSApplication", "requestUserAttention:", "send_isize_usize"),
    ("NSApplication", "respondsToSelector:", "send_bool_sel"),
    ("NSApplication", "effectiveAppearance", "send_id"),
    // Sends made by the `sendEvent:` override (app.rs): the key window for the
    // Cmd+keyUp fix, and the event's pointer deltas for device events.
    ("NSApplication", "keyWindow", "send_id"),
    ("NSWindow", "sendEvent:", "send_v_id"),
    ("NSEvent", "deltaX", "send_f64"),
    ("NSEvent", "deltaY", "send_f64"),
    // ---- NSScreen ----
    ("NSScreen", "frame", "send_rect"),
    ("NSScreen", "backingScaleFactor", "send_f64"),
    ("NSScreen", "visibleFrame", "send_rect"),
    // ---- the rest ----
    ("NSAppearance", "name", "send_id"),
    (
        "NSAppearance",
        "bestMatchFromAppearancesWithNames:",
        "send_id_id",
    ),
    ("NSButton", "setHidden:", "send_v_bool"),
    ("NSButton", "setEnabled:", "send_v_bool"),
    ("NSButton", "isEnabled", "send_bool"),
    ("NSPasteboard", "propertyListForType:", "send_id_id"),
    ("NSArray", "count", "send_usize"),
    ("NSArray", "objectAtIndex:", "send_id_usize"),
    ("NSDictionary", "objectForKey:", "send_id_id"),
    ("NSString", "isEqualToString:", "send_bool_id"),
    (
        "NSView",
        "setWantsBestResolutionOpenGLSurface:",
        "send_v_bool",
    ),
    ("NSView", "setWantsLayer:", "send_v_bool"),
    ("NSView", "window", "send_id"),
    // ---- W10, event.rs ----
    ("NSEvent", "keyCode", "send_u16"),
    ("NSEvent", "modifierFlags", "send_usize"),
    ("NSEvent", "characters", "send_id"),
    ("NSEvent", "charactersIgnoringModifiers", "send_id"),
    // ---- W9 phase 2, cursor.rs ----
    (
        "NSBitmapImageRep",
        "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:\
         isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:",
        "send_id_planeptr_isize_isize_isize_isize_bool_bool_id_isize_isize",
    ),
    ("NSBitmapImageRep", "bitmapData", "send_charptr"),
    ("NSImage", "initWithSize:", "send_id_size"),
    ("NSImage", "addRepresentation:", "send_v_id"),
    ("NSImage", "initByReferencingFile:", "send_id_id"),
    ("NSImage", "initWithData:", "send_id_id"),
    ("NSCursor", "initWithImage:hotSpot:", "send_id_id_point"),
    ("NSString", "stringByAppendingPathComponent:", "send_id_id"),
    ("NSObject", "isKindOfClass:", "send_bool_cls"),
    ("NSObject", "performSelector:", "send_id_sel"),
    ("NSNumber", "doubleValue", "send_f64"),
    ("NSData", "initWithBytes:length:", "send_id_cptr_usize"),
    // The two `CustomCursor` comparisons (see `cursor.rs` for why they are
    // sends and not pointer comparisons).
    ("NSObject", "isEqual:", "send_bool_id"),
    ("NSObject", "hash", "send_usize"),
    // ---- W9 phase 2, view.rs ----
    ("NSView", "bounds", "send_rect"),
    ("NSView", "frame", "send_rect"),
    ("NSView", "addCursorRect:cursor:", "send_v_rect_id"),
    ("NSView", "inputContext", "send_id"),
    ("NSView", "convertRect:toView:", "send_rect_rect_id"),
    ("NSView", "convertPoint:fromView:", "send_point_point_id"),
    ("NSView", "removeTrackingRect:", "send_v_isize"),
    (
        "NSView",
        "addTrackingRect:owner:userData:assumeInside:",
        "send_isize_rect_id_ptr_bool",
    ),
    ("NSView", "interpretKeyEvents:", "send_v_id"),
    (
        "NSView",
        "setPostsFrameChangedNotifications:",
        "send_v_bool",
    ),
    ("NSTextInputContext", "discardMarkedText", "send_v"),
    (
        "NSTextInputContext",
        "invalidateCharacterCoordinates",
        "send_v",
    ),
    (
        "NSTextInputContext",
        "selectedKeyboardInputSource",
        "send_id",
    ),
    ("NSAttributedString", "length", "send_usize"),
    ("NSAttributedString", "string", "send_id"),
    ("NSString", "copy", "send_id"),
    ("NSString", "substringToIndex:", "send_id_usize"),
    // `-length` counts UTF-16 code units and `-lengthOfBytesUsingEncoding:`
    // UTF-8 bytes; `view.rs` needs both, so each has its own row.
    ("NSString", "length", "send_usize"),
    (
        "NSString",
        "lengthOfBytesUsingEncoding:",
        "send_usize_usize",
    ),
    (
        "NSNotificationCenter",
        "addObserver:selector:name:object:",
        "send_v_id_sel_id_id",
    ),
    ("NSWindow", "convertRectToScreen:", "send_rect_rect"),
    ("NSWindow", "firstResponder", "send_id"),
    ("NSWindow", "selectNextKeyView:", "send_v_id"),
    ("NSWindow", "selectPreviousKeyView:", "send_v_id"),
    ("NSEvent", "isARepeat", "send_bool"),
    ("NSEvent", "scrollingDeltaX", "send_f64"),
    ("NSEvent", "scrollingDeltaY", "send_f64"),
    ("NSEvent", "hasPreciseScrollingDeltas", "send_bool"),
    ("NSEvent", "momentumPhase", "send_usize"),
    ("NSEvent", "phase", "send_usize"),
    // Adjacent `NSEvent` properties of different float widths: `d` and `f`.
    ("NSEvent", "magnification", "send_f64"),
    ("NSEvent", "rotation", "send_f32"),
    ("NSEvent", "pressure", "send_f32"),
    ("NSEvent", "stage", "send_isize"),
    ("NSEvent", "buttonNumber", "send_isize"),
    ("NSEvent", "locationInWindow", "send_point"),
    ("NSEvent", "type", "send_usize"),
    ("NSEvent", "timestamp", "send_f64"),
    ("NSEvent", "windowNumber", "send_isize"),
    ("NSEvent", "copy", "send_id"),
    // Ten arguments, four on the stack on AAPCS64.
    (
        "NSBitmapImageRep",
        "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:",
        "send_id_planeptr_isize_isize_isize_isize_bool_bool_id_isize_isize",
    ),
    // ---- W12, menu.rs ----
    ("NSMenu", "addItem:", "send_v_id"),
    ("NSMenuItem", "setSubmenu:", "send_v_id"),
    (
        "NSMenuItem",
        "initWithTitle:action:keyEquivalent:",
        "send_id_id_sel_id",
    ),
    // `NSEventModifierFlags` is an `NSUInteger`, so the setter is `v@:Q`.
    (
        "NSMenuItem",
        "setKeyEquivalentModifierMask:",
        "send_v_usize",
    ),
    ("NSProcessInfo", "processName", "send_id"),
    ("NSString", "stringByAppendingString:", "send_id_id"),
    ("NSApplication", "setServicesMenu:", "send_v_id"),
    ("NSApplication", "setMainMenu:", "send_v_id"),
    // ---- W12, monitor.rs ----
    ("NSScreen", "deviceDescription", "send_id"),
    // `I`, not `Q`. See the `u32` token above.
    ("NSNumber", "unsignedIntValue", "send_u32"),
    // ---- app_state.rs and event_loop.rs ----
    //
    // `-setActivationPolicy:` returns a `BOOL` the fork ignores, and takes a
    // signed `NSApplicationActivationPolicy`.
    ("NSApplication", "setActivationPolicy:", "send_bool_isize"),
    ("NSApplication", "delegate", "send_id"),
    ("NSApplication", "windows", "send_id"),
    ("NSApplication", "run", "send_v"),
    ("NSApplication", "stop:", "send_v_id"),
    ("NSApplication", "postEvent:atStart:", "send_v_id_bool"),
    ("NSRunningApplication", "bundleIdentifier", "send_id"),
    ("NSWindow", "close", "send_v"),
    // Target-action selectors `menu.rs` hands to AppKit that `event_loop.rs`'s
    // `hide_application` / `hide_other_applications` also send directly.
    ("NSApplication", "hide:", "send_v_id"),
    ("NSApplication", "hideOtherApplications:", "send_v_id"),
    // ---- W12, app.rs's swizzled -sendEvent: ----
    ("NSApplication", "keyWindow", "send_id"),
    ("NSEvent", "deltaX", "send_f64"),
    ("NSEvent", "deltaY", "send_f64"),
    // The `sendEvent:` override forwards a Cmd-modified key-up with
    // `send_v_id(key_window, sel!(sendEvent:), event)`.
    ("NSWindow", "sendEvent:", "send_v_id"),
];

/// The CLASS-METHOD rows. `class_getInstanceMethod` cannot see these.
const CLASS_ROWS: &[(&str, &str, &str)] = &[
    ("NSApplication", "sharedApplication", "send_id"),
    ("NSScreen", "mainScreen", "send_id"),
    ("NSColor", "clearColor", "send_id"),
    ("NSColor", "windowBackgroundColor", "send_id"),
    ("NSAppearance", "appearanceNamed:", "send_id_id"),
    ("NSArray", "arrayWithObjects:count:", "send_id_idptr_usize"),
    // W10, event.rs's `dummy_event`. The row that made `i16` necessary.
    (
        "NSEvent",
        "otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:\
         data2:",
        "send_id_usize_point_usize_f64_isize_id_i16_isize_isize",
    ),
    // ---- W9 phase 2 ----
    (
        "NSDictionary",
        "dictionaryWithContentsOfFile:",
        "send_id_id",
    ),
    ("NSEvent", "pressedMouseButtons", "send_usize"),
    ("NSNotificationCenter", "defaultCenter", "send_id"),
    ("NSMutableAttributedString", "new", "send_id"),
    ("NSArray", "new", "send_id"),
    (
        "NSEvent",
        "keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:\
         charactersIgnoringModifiers:isARepeat:keyCode:",
        "send_id_usize_point_usize_f64_isize_id_id_id_bool_u16",
    ),
    // `cursor.rs`'s documented cursor accessors, each listed: all `@16@0:8`,
    // all +0 autoreleased.
    ("NSCursor", "arrowCursor", "send_id"),
    ("NSCursor", "pointingHandCursor", "send_id"),
    ("NSCursor", "openHandCursor", "send_id"),
    ("NSCursor", "closedHandCursor", "send_id"),
    ("NSCursor", "IBeamCursor", "send_id"),
    ("NSCursor", "IBeamCursorForVerticalLayout", "send_id"),
    ("NSCursor", "dragCopyCursor", "send_id"),
    ("NSCursor", "dragLinkCursor", "send_id"),
    ("NSCursor", "operationNotAllowedCursor", "send_id"),
    ("NSCursor", "contextualMenuCursor", "send_id"),
    ("NSCursor", "crosshairCursor", "send_id"),
    ("NSCursor", "resizeRightCursor", "send_id"),
    ("NSCursor", "resizeUpCursor", "send_id"),
    ("NSCursor", "resizeLeftCursor", "send_id"),
    ("NSCursor", "resizeDownCursor", "send_id"),
    ("NSCursor", "resizeLeftRightCursor", "send_id"),
    ("NSCursor", "resizeUpDownCursor", "send_id"),
    // The two `+[NSEvent …]` factories. `otherEventWithType:` carries
    // `subtype:` as a signed short (`@@:Q{CGPoint=dd}Qdq@sqq`).
    (
        "NSEvent",
        "keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:",
        "send_id_usize_point_usize_f64_isize_id_id_id_bool_u16",
    ),
    (
        "NSEvent",
        "otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:",
        "send_id_usize_point_usize_f64_isize_id_i16_isize_isize",
    ),
    // ---- menu.rs ----
    //
    // `+new` is listed per class: the selector-name coverage arm cannot tell
    // `+[NSMenu new]` from another class's `new`
    // (`every_class_object_receiver_is_censused_for_its_own_class` checks
    // receivers).
    ("NSMenu", "new", "send_id"),
    ("NSMenuItem", "new", "send_id"),
    ("NSMenuItem", "separatorItem", "send_id"),
    ("NSProcessInfo", "processInfo", "send_id"),
    // ---- W12, monitor.rs ----
    ("NSScreen", "screens", "send_id"),
    // ---- W12, app_state.rs and event_loop.rs ----
    ("NSRunningApplication", "currentApplication", "send_id"),
    // `NSWindow` class methods: the receiver is the class object.
    (
        "NSWindow",
        "setAllowsAutomaticWindowTabbing:",
        "send_v_bool",
    ),
    ("NSWindow", "allowsAutomaticWindowTabbing", "send_bool"),
];

/// Rows the fork sends through an `aterm_objc::msg()` function pointer rather
/// than a named `send_*` helper. The third field is a helper-style spelling of
/// the fn type, from which [`shape_of`] derives the expected encoding as for a
/// real helper.
///
/// `window_delegate.rs` declares
/// `unsafe extern "C-unwind" fn(Id, Sel, CGRect, usize, usize, Bool) -> Id`.
const MSG_POINTER_ROWS: &[(&str, &str, &str)] = &[
    (
        "NSWindow",
        "initWithContentRect:styleMask:backing:defer:",
        "send_id_rect_usize_usize_bool",
    ),
    // `app_state.rs`'s `as_delegate_id` checks conformance to
    // `NSApplicationDelegate` with
    // `unsafe extern "C-unwind" fn(Id, Sel, ProtocolPtr) -> Bool`. `Protocol *`
    // encodes `@`, not `#`.
    ("NSObject", "conformsToProtocol:", "send_bool_proto"),
];

/// Selectors the fork spells with `sel!` but never sends, each with the reason.
///
/// `sel!` also names target-action selectors and swizzle lookups. The coverage
/// test checks this list both ways: an entry no longer in the fork fails, and
/// an entry whose selector appears in a send position fails.
const NOT_SENT: &[(&str, &str)] = &[
    (
        "orderFrontStandardAboutPanel:",
        "menu.rs:72 — a target-action selector handed to `menu_item`; AppKit \
         dispatches it, the fork never does",
    ),
    (
        "unhideAllApplications:",
        "menu.rs:109 — the Show All item's action, handed to `menu_item` for AppKit to \
         dispatch; the fork never sends it",
    ),
    (
        "terminate:",
        "menu.rs:119 — the Quit item's action, handed to `menu_item` for AppKit to \
         dispatch; the fork never sends it",
    ),
    (
        "frameDidChange:",
        "view.rs:1073 — the `selector:` ARGUMENT of \
         `addObserver:selector:name:object:`. `WinitView` DECLARES it \
         (view.rs:244) and `winit_seam.rs` censuses that declaration; the \
         notification centre sends it, not the fork",
    ),
];

/// `cursor.rs`'s undocumented cursor accessors. `try_cursor_from_selector`
/// asks `-respondsToSelector:` first and falls back to the arrow cursor, so
/// the rule is conditional (if it resolves, it must encode `send_id`) plus a
/// floor on how many resolve.
const TOLERATED_CLASS_ROWS: &[&str] = &[
    "_helpCursor",
    "_zoomInCursor",
    "_zoomOutCursor",
    "_windowResizeNorthEastCursor",
    "_windowResizeNorthWestCursor",
    "_windowResizeSouthEastCursor",
    "_windowResizeSouthWestCursor",
    "_windowResizeNorthEastSouthWestCursor",
    "_windowResizeNorthWestSouthEastCursor",
    "busyButClickableCursor",
];

fn leak_cstr(name: &str) -> &'static CStr {
    Box::leak(
        CString::new(name)
            .expect("no interior NUL")
            .into_boxed_c_str(),
    )
}

/// The live encoding of `cls`'s instance or class method `name`.
fn live_encoding(cls: &str, name: &str, class_method: bool) -> Option<String> {
    let c = class(leak_cstr(cls));
    let s = sel_uncached(leak_cstr(name));
    let types = if class_method {
        // SAFETY: `c` is a live registered class and `s` an interned selector;
        // `class_getClassMethod` tolerates one the metaclass does not implement.
        unsafe { aterm_objc::method_types(aterm_objc::class_of(c.as_id()), s) }
    } else {
        // SAFETY: as above, for the instance side.
        unsafe { aterm_objc::method_types(c, s) }
    };
    types.map(|t| strip_method_offsets(&t))
}

#[test]
fn every_sent_selector_encodes_the_way_its_helper_spells_it() {
    assert!(
        load_appkit(),
        "AppKit is not present: this census has no authority to consult"
    );

    let mut checked = 0_usize;
    let mut findings = Vec::new();
    for &(cls, name, helper) in ROWS.iter().chain(CLASS_ROWS).chain(MSG_POINTER_ROWS) {
        let is_class = CLASS_ROWS.iter().any(|r| r.0 == cls && r.1 == name);
        let want = shape_of(helper);
        match live_encoding(cls, name, is_class) {
            Some(got) if got == want => checked += 1,
            Some(got) => findings.push(format!(
                "{cls} {name}: helper {helper} spells {want:?}, runtime says {got:?}"
            )),
            None => findings.push(format!("{cls} {name}: selector does not resolve")),
        }
    }
    assert!(
        findings.is_empty(),
        "{} disagreement(s):\n  {}",
        findings.len(),
        findings.join("\n  ")
    );
    assert_eq!(
        checked,
        ROWS.len() + CLASS_ROWS.len() + MSG_POINTER_ROWS.len(),
        "every row must have been consulted, not merely not-disagreed-with"
    );
    assert!(
        checked >= 216,
        "the census shrank to {checked} rows; it covered 96 when written and \
         179 after W9 phase 2 added `cursor.rs`'s and `view.rs`'s. The floor is \
         the MEASURED count, not a guess: 187 was written here first, from \
         counting the rows by eye, and this assertion is what said 179. Pass 14 \
         raised it to 183 — four sends the fork really makes had no row at all, \
         which is why a floor on the TABLE is not coverage of the SOURCE. W12 \
         raised it to 202 with `menu.rs`'s twelve, `monitor.rs`'s three \
         `app.rs`'s four, and \
         `app_state.rs`/`event_loop.rs`/`window.rs`'s twelve, and added a THIRD arm — \
         `every_class_object_receiver_is_censused_for_its_own_class` — \
         because the coverage arm matches selector NAMES and so two of those \
         twelve (`+[NSMenu new]`, `+[NSMenuItem new]`) were already \
         'covered' by `new` rows belonging to other classes."
    );
}

/// Every selector the fork spells is a row here or a declared non-send. The row
/// count alone cannot notice a new `sel!` with no row.
#[test]
fn every_selector_the_fork_sends_has_a_census_row() {
    let mut seen: Vec<(String, String)> = Vec::new();
    // RECURSIVE — `fork_rs_files` says why.
    let sources = fork_rs_files();
    let files = sources.len();
    for (file, src) in &sources {
        for (i, line) in src.lines().enumerate() {
            // Strip `//` comments: this file's own prose names selectors.
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(at) = rest.find("sel!(") {
                rest = &rest[at + "sel!(".len()..];
                // A selector holds no `)`, so the first one closes the call. A
                // multi-line `sel!(\n foo:\n)` is collected below from the joined
                // form instead.
                let Some(end) = rest.find(')') else { break };
                let name = rest[..end].trim();
                rest = &rest[end..];
                if name.is_empty() || name.starts_with('$') {
                    continue; // `sel!($name)` — the macro_rules parameter.
                }
                seen.push((name.to_owned(), format!("{file}:{}", i + 1)));
            }
        }
        // The multi-line form: `sel!(` alone on its line, selector on the next.
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if !code.trim_end().ends_with("sel!(") {
                continue;
            }
            let name = lines
                .get(i + 1)
                .map(|l| l.trim().trim_end_matches(')').trim())
                .unwrap_or("");
            if !name.is_empty() && !name.starts_with('$') {
                seen.push((name.to_owned(), format!("{file}:{}", i + 2)));
            }
        }
    }
    assert!(
        files >= 10 && seen.len() >= 200,
        "the walk found {files} file(s) and {} `sel!` site(s) — it is not \
         reading the fork, and a coverage test that reads nothing passes",
        seen.len()
    );

    let covered: Vec<&str> = ROWS
        .iter()
        .chain(CLASS_ROWS)
        .chain(MSG_POINTER_ROWS)
        .map(|r| r.1)
        .chain(TOLERATED_CLASS_ROWS.iter().copied())
        .chain(NO_STATIC_ENCODING.iter().map(|r| r.1))
        .chain(NOT_SENT.iter().map(|r| r.0))
        .collect();

    let mut uncovered: Vec<String> = seen
        .iter()
        .filter(|(name, _)| !covered.contains(&name.as_str()))
        .map(|(name, at)| format!("{at}: {name}"))
        .collect();
    uncovered.sort();
    uncovered.dedup();
    assert!(
        uncovered.is_empty(),
        "{} selector(s) the fork spells have no census row and no `NOT_SENT` \
         entry — add the prototype row, or record WHY it is never sent:\n  {}",
        uncovered.len(),
        uncovered.join("\n  ")
    );

    // Both ways: an excuse that no longer names a live site must go, or it
    // pre-approves a future send of the same selector.
    let stale: Vec<&str> = NOT_SENT
        .iter()
        .map(|r| r.0)
        .filter(|n| !seen.iter().any(|(name, _)| name == n))
        .collect();
    assert!(
        stale.is_empty(),
        "these `NOT_SENT` entries name selectors the fork no longer spells: \
         {stale:?}"
    );
    for (name, reason) in NOT_SENT {
        assert!(
            reason.len() >= 40,
            "the `NOT_SENT` entry for {name} must say why, not just that"
        );
    }

    // An excuse is checked against use, not presence: a selector the fork
    // spells as a lookup and also sends (e.g. `sendEvent:`) must not be
    // excused. `sent_selectors` reads the position: a `sel!` in the second
    // argument of any call, whatever the callee is called, since that is where
    // `objc_msgSend`'s `_cmd` goes. `addObserver:selector:name:object:` passes
    // `frameDidChange:` as argument four, which is correctly not a send.
    let sent = sent_selectors();

    // First, every `msg()` cast site found in the source must be seen by the
    // walk, so a regression names the sites rather than tripping a count.
    let cast_sends = raw_msg_cast_selectors();
    assert!(
        cast_sends.len() >= 3,
        "the fork's raw `msg()` cast sites were not found ({} of them); this \
         arm is not reading the fork",
        cast_sends.len()
    );
    let unseen: Vec<String> = cast_sends
        .iter()
        .filter(|(name, _)| !sent.iter().any(|s| s == name))
        .map(|(name, at)| format!("{at}: {name}"))
        .collect();
    assert!(
        unseen.is_empty(),
        "{} send(s) made through a raw `msg()` cast are invisible to the \
         send-position walk, so an excuse for any of them could go false \
         without this test noticing:\n  {}",
        unseen.len(),
        unseen.join("\n  ")
    );

    // …and only then the floor, which is a ratchet on the same reading.
    assert!(
        sent.len() >= 185,
        "the send-position walk found only {} selector(s); it is not reading \
         the fork",
        sent.len()
    );

    let contradicted: Vec<&str> = NOT_SENT
        .iter()
        .map(|r| r.0)
        .filter(|n| sent.iter().any(|s| s == n))
        .collect();
    assert!(
        contradicted.is_empty(),
        "these `NOT_SENT` entries name selectors the fork DOES send — the \
         excuse became false without the selector disappearing, which is \
         exactly what the presence check above cannot see: {contradicted:?}"
    );
}

/// Every selector in a send position in the fork: a `sel!(…)` that is the
/// second argument of a call, whatever the callee (`send_v_id(…)`, a raw
/// `msg()` cast's `f(…)`, `SEND_EVENT.install(…)`), since that is where
/// `objc_msgSend` puts `_cmd`.
fn sent_selectors() -> Vec<String> {
    let mut out = Vec::new();
    for (_file, stripped) in fork_sources() {
        out.extend(
            selectors_in_second_argument_position(&stripped)
                .into_iter()
                .map(|(n, _)| n),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// One frame of the argument-position walk.
struct Frame {
    /// `(` preceded by an identifier character — i.e. a CALL, not a grouping
    /// paren, a tuple or a `&[…]`.
    is_call: bool,
    /// How many top-level commas have been seen inside this frame.
    arg: usize,
    /// Whether only whitespace and a path qualifier have appeared since this
    /// argument began, so both `sel!` and `aterm_objc::sel!` count as the
    /// start of argument two.
    at_arg_start: bool,
}

/// Every `sel!(…)` that begins the second argument of a call, with its line.
/// One pass with an explicit frame stack; `[` and `{` push frames too, so a
/// comma inside a slice or block does not advance the call's argument index.
fn selectors_in_second_argument_position(stripped: &str) -> Vec<(String, usize)> {
    let b: Vec<char> = stripped.chars().collect();
    let mut out = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0_usize;
    let mut line = 1_usize;
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        // A `sel!(…)` — read the selector, decide, and jump past it whole so
        // its own parentheses never reach the frame stack.
        if c == 's' && stripped[byte_at(&b, i)..].starts_with("sel!(") {
            let rest = &stripped[byte_at(&b, i) + "sel!(".len()..];
            let end = rest.find(')');
            let name = end.map(|e| rest[..e].trim().to_owned()).unwrap_or_default();
            let is_selector_argument = stack
                .last()
                .is_some_and(|f| f.is_call && f.arg == 1 && f.at_arg_start);
            if is_selector_argument && !name.is_empty() && !name.starts_with('$') {
                out.push((name, line));
            }
            // Advance past the whole macro call, counting newlines inside it
            // (a multi-line `sel!(\n  foo:bar:\n)` is the fork's own style).
            let Some(e) = end else { break };
            let consumed: usize = "sel!(".chars().count() + rest[..=e].chars().count();
            line += b[i..(i + consumed).min(b.len())]
                .iter()
                .filter(|c| **c == '\n')
                .count();
            i += consumed;
            if let Some(f) = stack.last_mut() {
                f.at_arg_start = false;
            }
            continue;
        }
        // A string or char literal: skip it whole, so a comma or paren inside
        // one cannot move the argument index.
        if c == '"' {
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' {
                    i += 1;
                }
                if i < b.len() && b[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 1;
            if let Some(f) = stack.last_mut() {
                f.at_arg_start = false;
            }
            continue;
        }
        match c {
            '(' => {
                let prev = b[..i].iter().rev().find(|p| !p.is_whitespace());
                let is_call = prev.is_some_and(|p| p.is_alphanumeric() || *p == '_' || *p == '>');
                stack.push(Frame {
                    is_call,
                    arg: 0,
                    at_arg_start: true,
                });
            }
            '[' | '{' => stack.push(Frame {
                is_call: false,
                arg: 0,
                at_arg_start: true,
            }),
            ')' | ']' | '}' => {
                stack.pop();
                if let Some(f) = stack.last_mut() {
                    f.at_arg_start = false;
                }
            }
            ',' => {
                if let Some(f) = stack.last_mut() {
                    f.arg += 1;
                    f.at_arg_start = true;
                }
            }
            // A path qualifier — `aterm_objc::` — leaves the argument still
            // "at its start"; anything else does not.
            ch if ch.is_alphanumeric() || ch == '_' || ch == ':' => {}
            ch if !ch.is_whitespace() => {
                if let Some(f) = stack.last_mut() {
                    f.at_arg_start = false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// `text` up to and including the `)` that closes its first `(`, so a lookup
/// cannot match a later call.
fn call_span(text: &str) -> &str {
    let mut depth = 0_usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[..=i];
                }
            }
            _ => {}
        }
    }
    text
}

/// Sends made through a raw `msg()` cast: `let f: unsafe extern "C-unwind"
/// fn(Id, Sel, …) = msg();` then `f(recv, sel!(x), …)`. Finds each binding and
/// the selector at its call, as an independent set for the check above.
fn raw_msg_cast_selectors() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (file, stripped) in fork_sources() {
        let mut from = 0_usize;
        while let Some(at) = stripped[from..].find("msg()") {
            let start = from + at;
            from = start + "msg()".len();
            // The binding: `let <name>: unsafe extern "C-unwind" fn(…) = …msg();`
            let Some(let_at) = stripped[..start].rfind("let ") else {
                continue;
            };
            let head = &stripped[let_at + "let ".len()..];
            let Some(colon) = head.find(':') else {
                continue;
            };
            let name = head[..colon].trim();
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            // The call: the first `<name>(` after the cast.
            let tail = &stripped[from..];
            let mut search = 0_usize;
            let call_at = loop {
                let Some(hit) = tail[search..].find(&format!("{name}(")) else {
                    break None;
                };
                let abs = search + hit;
                let prev = tail[..abs].chars().next_back();
                if prev.is_none_or(|p| !(p.is_alphanumeric() || p == '_')) {
                    break Some(abs);
                }
                search = abs + 1;
            };
            let Some(call_at) = call_at else { continue };
            let line = stripped[..from + call_at].matches('\n').count() + 1;
            // Bound the span to this call, or the next send in the file answers.
            let span = call_span(&tail[call_at..]);
            // An independent reading: the first `sel!` in the span by
            // substring, not the frame-stack walk under test. The cast type
            // `fn(Id, Sel, …)` makes argument two the selector.
            if let Some(at) = span.find("sel!(") {
                let rest = &span[at + "sel!(".len()..];
                if let Some(end) = rest.find(')') {
                    let name = rest[..end].trim();
                    if !name.is_empty() {
                        out.push((name.to_owned(), format!("{file}:{line}")));
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The byte offset of char index `i` in the string `b` was built from.
fn byte_at(b: &[char], i: usize) -> usize {
    b[..i].iter().map(|c| c.len_utf8()).sum()
}

/// Class-object receivers whose selector is not a literal at the send, each
/// with the reason and what is censused instead.
const DYNAMIC_CLASS_RECEIVERS: &[(&str, &str)] = &[
    (
        "cursor.rs",
        "`cursor_from_selector(s)` sends a Sel its callers pass in. Every one \
         of the 27 literals that reach it is censused — the 17 documented \
         accessors in `CLASS_ROWS`, the 10 undocumented ones in \
         `TOLERATED_CLASS_ROWS` — and all 27 are `send_id`.",
    ),
    (
        "window_delegate.rs",
        "`set_transparent` picks `+clearColor` or `+windowBackgroundColor` \
         into a local before the send. Both are `CLASS_ROWS` entries on \
         `NSColor` and both are `send_id`.",
    ),
];

/// A send to a class object is censused against its own class.
///
/// [`every_selector_the_fork_sends_has_a_census_row`] matches selector names
/// only, which cannot distinguish `+[NSMenu new]` from another class's `new`.
/// This reads `class(c"X").as_id()` receivers from the source and checks each
/// `(class, selector)` pair against the table.
#[test]
fn every_class_object_receiver_is_censused_for_its_own_class() {
    let mut pairs: Vec<(String, String, String)> = Vec::new();
    let mut dynamic: Vec<String> = Vec::new();

    // `//` comments stripped: this file's own prose spells the pattern, and so
    // does the fork's.
    for (file, stripped) in fork_sources() {
        let mut from = 0_usize;
        while let Some(at) = stripped[from..].find("class(c\"") {
            let start = from + at;
            let rest = &stripped[start..];
            let name_start = "class(c\"".len();
            let Some(name_end) = rest[name_start..].find('"') else {
                break;
            };
            let cls = &rest[name_start..name_start + name_end];
            let after = &rest[name_start + name_end..];
            from = start + name_start + name_end;
            // Only the receiver spelling: `class(c"X")` without `.as_id()` is a
            // class pointer argument (`alloc`, `isKindOfClass:`), not a send.
            let Some(tail) = after.strip_prefix("\").as_id()") else {
                continue;
            };
            let line = stripped[..start].matches('\n').count() + 1;
            let t = tail.trim_start_matches([',', ' ', '\n', '\t']);
            if let Some(selrest) = t.strip_prefix("sel!(") {
                let Some(end) = selrest.find(')') else {
                    continue;
                };
                let name = selrest[..end].trim().to_owned();
                pairs.push((cls.to_owned(), name, format!("{file}:{line}")));
            } else {
                dynamic.push(file.clone());
            }
        }
    }

    assert!(
        pairs.len() >= 20,
        "the walk found only {} class-object receiver(s); it is not reading \
         the fork",
        pairs.len()
    );

    let covered: Vec<(&str, &str)> = CLASS_ROWS
        .iter()
        .map(|r| (r.0, r.1))
        .chain(TOLERATED_CLASS_ROWS.iter().map(|n| ("NSCursor", *n)))
        .chain(NO_STATIC_ENCODING.iter().map(|r| (r.0, r.1)))
        .collect();
    let missing: Vec<String> = pairs
        .iter()
        .filter(|(cls, name, _)| !covered.contains(&(cls.as_str(), name.as_str())))
        .map(|(cls, name, at)| format!("{at}: +[{cls} {name}]"))
        .collect();
    assert!(
        missing.is_empty(),
        "{} class-method send(s) have no CLASS_ROWS entry FOR THEIR OWN \
         CLASS — a row for the same selector on a different class does not \
         check these:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );

    // Dynamic sites are checked both ways: an excuse whose file no longer has
    // one fails.
    dynamic.sort();
    dynamic.dedup();
    let excused: Vec<&str> = DYNAMIC_CLASS_RECEIVERS.iter().map(|r| r.0).collect();
    let unexcused: Vec<&String> = dynamic
        .iter()
        .filter(|f| !excused.contains(&f.as_str()))
        .collect();
    assert!(
        unexcused.is_empty(),
        "these files send to a class object with a non-literal selector and \
         have no entry in `DYNAMIC_CLASS_RECEIVERS`: {unexcused:?}"
    );
    let stale: Vec<&str> = excused
        .iter()
        .copied()
        .filter(|f| !dynamic.iter().any(|d| d == f))
        .collect();
    assert!(
        stale.is_empty(),
        "these `DYNAMIC_CLASS_RECEIVERS` entries name files with no such site \
         any more: {stale:?}"
    );
    for (file, reason) in DYNAMIC_CLASS_RECEIVERS {
        assert!(
            reason.len() >= 60,
            "the `DYNAMIC_CLASS_RECEIVERS` entry for {file} must say what IS \
             censused instead"
        );
    }
}

/// The undocumented cursors: conditional encoding, with a floor on how many
/// are really there.
#[test]
fn the_undocumented_cursor_accessors_encode_the_way_the_fork_sends_them() {
    assert!(load_appkit(), "AppKit is not present");
    let want = shape_of("send_id");
    let mut resolved = 0_usize;
    let mut findings = Vec::new();
    for name in TOLERATED_CLASS_ROWS {
        match live_encoding("NSCursor", name, true) {
            None => {} // withdrawn: `try_cursor_from_selector` falls back
            Some(got) if got == want => resolved += 1,
            Some(got) => findings.push(format!(
                "+[NSCursor {name}]: the fork sends {want:?}, runtime says {got:?}"
            )),
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n  "));
    assert_eq!(
        resolved,
        TOLERATED_CLASS_ROWS.len(),
        "only {resolved} of the {} undocumented cursor accessors resolve on this \
         macOS; that is not a failure of the fork (it falls back) but it IS the \
         moment to re-read the list rather than let it rot",
        TOLERATED_CLASS_ROWS.len()
    );
}

#[test]
fn the_protocol_row_encodes_the_way_its_helper_spells_it() {
    assert!(load_appkit(), "AppKit is not present");
    let p = aterm_objc::protocol(c"NSDraggingInfo");
    let s = sel_uncached(c"draggingPasteboard");
    // SAFETY: `p` is a live protocol handle and `s` an interned selector.
    let types = unsafe { aterm_objc::protocol_method_types(p, s, true) }
        .expect("-draggingPasteboard is required on NSDraggingInfo");
    assert_eq!(strip_method_offsets(&types), shape_of("send_id"));
}

#[test]
fn the_expected_shapes_are_not_all_the_same() {
    let mut shapes: Vec<String> = ROWS
        .iter()
        .chain(CLASS_ROWS)
        .map(|r| shape_of(r.2))
        .collect();
    shapes.sort();
    shapes.dedup();
    assert!(
        shapes.len() >= 20,
        "only {} distinct shapes: a census whose rows all spell the same encoding \
         cannot catch a row that spells the wrong one",
        shapes.len()
    );
}

#[test]
fn the_undecidable_rows_are_named_with_a_reason_and_stay_few() {
    assert_eq!(
        NO_STATIC_ENCODING.len(),
        4,
        "a row that cannot be checked statically must be argued for individually"
    );
    // The dynamic-property row really is invisible to the runtime's static
    // tables, which is what makes it an exception rather than an omission.
    assert!(load_appkit(), "AppKit is not present");
    assert!(
        live_encoding("NSWindowTabGroup", "setSelectedWindow:", false).is_none(),
        "`-setSelectedWindow:` became statically visible; check it in the census instead"
    );
    // The public cluster class implements neither initialiser; if Foundation
    // moves them onto it, they belong in the census instead.
    for name in ["initWithString:", "initWithAttributedString:"] {
        assert!(
            live_encoding("NSMutableAttributedString", name, false).is_none(),
            "`-[NSMutableAttributedString {name}]` became statically visible; \
             move it into the census instead of excusing it"
        );
    }
}
