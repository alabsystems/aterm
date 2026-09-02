// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W8 — the census of the BINDINGS, beside `winit_seam.rs`'s census of the
//! DECLARATIONS.
//!
//! # The gap this closes, and the defect that proved it was open
//!
//! `winit_seam.rs` asks whether the fork's DECLARED rows encode the way their
//! authority says. Nothing asked the same question of the rows the fork SENDS,
//! and W8 moved 177 of them off `objc2-app-kit` in one file. A binding crate
//! answered that question for free: `objc2-app-kit`'s generated
//! `-requestUserAttention:` carried the argument type from the SDK, so a caller
//! could not get it wrong. Written by hand, a caller can, and one did.
//!
//! `send_isize_isize(app, sel!(requestUserAttention:), ty)` was the first
//! spelling. The runtime's own answer is
//!
//! ```text
//! NSApplication  requestUserAttention:  q24@0:8Q16
//! ```
//!
//! — a SIGNED return over an UNSIGNED argument, a shape nothing else in either
//! ported file has. NOTHING WOULD HAVE CAUGHT IT. Both are 64-bit words on both
//! Apple ABIs, the only two values the fork passes are 0 and 10, every test
//! passed and every pixel was identical. It is the same class of defect as the
//! `NSTextAlignmentCenter` constant W7 shipped and the pixels caught, in the
//! one place pixels could never have looked.
//!
//! So the encodings are READ, not reasoned about. Each row below names the
//! helper the fork calls, and the expected encoding is DERIVED FROM THE
//! HELPER'S NAME rather than copied from the SDK — a copied string is a second
//! guess, and two guesses that agree are still a guess.
//!
//! # Non-vacuity
//!
//! Three ways this can fail to be a test, and each is closed:
//!
//! * No AppKit — [`load_appkit`] returns false and the test PANICS rather than
//!   passing with nothing consulted.
//! * A selector that does not resolve — counted, and the run fails if any row
//!   is unresolved except the ones named in [`NO_STATIC_ENCODING`] with a
//!   reason.
//! * A shape vocabulary that silently accepts everything — [`shape_of`] panics
//!   on a helper name it does not know, and
//!   [`the_expected_shapes_are_not_all_the_same`] asserts the table exercises
//!   more than one.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;

use aterm_objc::{class, sel_uncached, strip_method_offsets};

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
}

/// `RTLD_LAZY`.
const RTLD_LAZY: c_int = 0x1;

/// Load AppKit into this test process, once.
fn load_appkit() -> bool {
    use std::sync::OnceLock;
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let path = CString::new("/System/Library/Frameworks/AppKit.framework/AppKit")
            .expect("no interior NUL");
        // SAFETY: `dlopen` with a valid C path and `RTLD_LAZY`; the handle is
        // deliberately never closed — AppKit is process-lifetime once loaded.
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
        !handle.is_null()
    })
}

/// The Objective-C type encoding a `send_*` helper's name implies, as
/// `<return><self><_cmd><args…>`.
///
/// This is the half of the check that makes it a check: the expected string
/// comes from the HELPER the fork calls, so a row that names the wrong helper
/// is caught even when the SDK string was transcribed perfectly.
fn shape_of(helper: &str) -> String {
    // Return type, then argument types, keyed on the helper's own naming rule:
    // `send_<ret>[_<arg>…]`, where a missing `<ret>` reads as `id`.
    let rest = helper.strip_prefix("send").expect("a send helper");
    let mut parts = rest.split('_').filter(|p| !p.is_empty());
    let ret = parts.next().expect("a return token");
    let mut out = String::from(token(ret));
    out.push_str("@:");
    for a in parts {
        out.push_str(token(a));
    }
    out
}

/// One name from a helper's `send_a_b_c` spelling, as its encoding.
fn token(t: &str) -> &'static str {
    match t {
        "v" => "v",
        "id" => "@",
        "bool" => "B",
        "isize" => "q",
        "usize" => "Q",
        "f64" => "d",
        // The C `float`. `NSEvent`'s `-rotation` and `-pressure` are `f` while
        // `-magnification` beside them is `d`; on AAPCS64 the first two return
        // in `s0`, the low half of the `d0` the third uses, so the wrong
        // prototype reads a denormal rather than a wrong number.
        "f32" => "f",
        "u16" => "S",
        // The SIGNED short. `NSEventSubtype` is the only narrow signed
        // argument either ported file sends (`+otherEventWithType:…subtype:`),
        // and `S` vs `s` is exactly the distinction the census exists for.
        "i16" => "s",
        "sel" => ":",
        "cls" => "#",
        // TWO POINTER TOKENS, deliberately. `ptr` is a bare `void *` the
        // runtime hands back untouched (KVO's `context:`); `idptr` is a const
        // pointer to object pointers Foundation dereferences and retains
        // (`+arrayWithObjects:count:`). They encode differently and the census
        // caught a helper that had spelled them the same.
        "ptr" => "^v",
        "idptr" => "r^@",
        // W9 phase 2 — THREE MORE POINTER TOKENS, and the count is the point.
        // `ptr`/`idptr`/`cptr`/`charptr`/`planeptr` are five distinct
        // encodings for five things Rust would happily spell as one raw
        // pointer, and every one of them is live in the fork:
        //
        //   ptr      ^v    KVO's `context:` — opaque, never dereferenced
        //   idptr    r^@   `+arrayWithObjects:count:` — const, retained
        //   cptr     r^v   `-initWithBytes:length:` — const, copied
        //   charptr  *     `-bitmapData` — an interior `unsigned char *`
        //   planeptr ^*    `-initWithBitmapDataPlanes:…` — an ARRAY of those
        //
        // `charptr` is the surprising one: clang collapses `char *`,
        // `signed char *` and `unsigned char *` to a bare `*`, so the `^C` a
        // reader would compose is a string the runtime never emits.
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
    // `selectedWindow` is declared `T@"NSWindow",W,D` — a DYNAMIC property, so
    // `NSWindowTabGroup`'s own method list has no entry for it and
    // `class_getInstanceMethod` answers NULL until AppKit resolves one. The
    // setter of an object property is `v@:@` on every ABI; the row is driven by
    // `select_tab_at_index`.
    ("NSWindowTabGroup", "setSelectedWindow:", "send_v_id"),
    // `-draggingPasteboard` belongs to the `NSDraggingInfo` PROTOCOL, not to a
    // class, so it is checked against the protocol below instead.
    ("NSObject", "draggingPasteboard", "send_id"),
    // W9 phase 2 — THE CLASS CLUSTER. `NSMutableAttributedString` is a
    // Foundation class cluster: the public class implements neither
    // initialiser, and `class_getInstanceMethod` answers NULL for both because
    // its superclass `NSAttributedString` does not implement them either.
    // Measured:
    //
    //   NSMutableAttributedString instancesRespondTo initWithString: = 0
    //   superclass chain: NSMutableAttributedString NSAttributedString NSObject
    //   [[NSMutableAttributedString alloc] initWithString:@"hi"] class
    //     = NSConcreteMutableAttributedString   (responds = 1)
    //
    // The IMP lives on a PRIVATE concrete subclass that `+alloc` substitutes,
    // so there is no stable class this census could name. Both are `@@:@` on
    // every ABI and both are exercised for real, on the composition path, by
    // `objc_ime_drive`'s stages.
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
    // THE ROW THIS FILE EXISTS FOR. `q` out, `Q` in.
    ("NSApplication", "requestUserAttention:", "send_isize_usize"),
    ("NSApplication", "respondsToSelector:", "send_bool_sel"),
    ("NSApplication", "effectiveAppearance", "send_id"),
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
    // The two `CustomCursor` sends its derives used to hide. See the type's
    // note in `cursor.rs` for why they are sends and not pointer comparisons.
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
    // THE PAIR. `-length` counts UTF-16 code units and
    // `-lengthOfBytesUsingEncoding:` counts UTF-8 bytes; `objc2` spelled them
    // `len_utf16()` and `len()`, and `view.rs` calls both on one string three
    // lines apart. Two rows, two shapes, so a port that collapsed them into one
    // send fails here rather than in an IME nobody on this team types in.
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
    // THE OTHER PAIR, and the one this wave found. Adjacent properties, both
    // feeding a gesture event, both "a float" in Rust — and `d` against `f`.
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
    // W9 pass 14. `cursor.rs:103` sends this and NOTHING re-checked it: the
    // prototype was added in the same wave and the row was not, and the
    // floor assertion below counts ROWS, so it cannot notice a send that has
    // no row. Ten arguments, four of them on the stack on AAPCS64.
    (
        "NSBitmapImageRep",
        "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:",
        "send_id_planeptr_isize_isize_isize_isize_bool_bool_id_isize_isize",
    ),
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
    // `cursor.rs`'s seventeen DOCUMENTED cursor accessors. All `@16@0:8`, all
    // +0 autoreleased, and listed individually rather than as one representative
    // because "they are all the same shape" is the claim being checked.
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
    // W9 pass 14 — the two `+[NSEvent …]` factories, both uncovered until now.
    // `otherEventWithType:` is the row that carries `subtype:` as a SIGNED
    // short (`s`, not `S`); the runtime says `@@:Q{CGPoint=dd}Qdq@sqq` and the
    // value the fork passes is 0, so no run could have caught the wrong sign.
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
];

/// Rows the fork sends through an `aterm_objc::msg()` FUNCTION POINTER rather
/// than a named `send_*` helper.
///
/// For these the Rust fn type IS the prototype, so the third field is a
/// helper-style SPELLING of that type rather than the name of a helper that
/// exists — [`shape_of`] derives the expected encoding from it exactly as it
/// does for a real helper, and the check against the runtime is the same
/// strength. It is a separate table so that a reader who greps for the name in
/// `aterm-objc` and finds nothing is not left wondering.
///
/// `window_delegate.rs:837` declares
/// `unsafe extern "C" fn(Id, Sel, CGRect, usize, usize, Bool) -> Id`.
const MSG_POINTER_ROWS: &[(&str, &str, &str)] = &[(
    "NSWindow",
    "initWithContentRect:styleMask:backing:defer:",
    "send_id_rect_usize_usize_bool",
)];

/// Selectors the fork spells with `sel!` but NEVER SENDS, each with the reason.
///
/// # Why this list has to exist
///
/// [`every_selector_the_fork_sends_has_a_census_row`] reads the fork's own
/// sources, and `sel!` has three uses in this codebase, only one of which is a
/// send: dispatching a message, naming a TARGET-ACTION selector for AppKit to
/// dispatch later, and looking a method up for a swizzle. A census that
/// demanded a prototype for all three would be wrong; one that guessed which
/// was which from surrounding syntax would be a parser nobody could trust. So
/// the classification is a decision, written down, with the site that justifies
/// it — and the coverage test checks this list BOTH ways, so an entry that
/// stops appearing in the fork dies rather than silently excusing a future send
/// of the same name.
const NOT_SENT: &[(&str, &str)] = &[
    (
        "orderFrontStandardAboutPanel:",
        "menu.rs:24 — a target-action selector handed to `menu_item`; AppKit \
         dispatches it, the fork never does",
    ),
    (
        "hide:",
        "menu.rs:39 — the Hide item's action, handed to `menu_item` for AppKit to \
         dispatch; the fork never sends it",
    ),
    (
        "hideOtherApplications:",
        "menu.rs:48 — the Hide Others item's action, handed to `menu_item` for AppKit to \
         dispatch; the fork never sends it",
    ),
    (
        "unhideAllApplications:",
        "menu.rs:61 — the Show All item's action, handed to `menu_item` for AppKit to \
         dispatch; the fork never sends it",
    ),
    (
        "terminate:",
        "menu.rs:71 — the Quit item's action, handed to `menu_item` for AppKit to \
         dispatch; the fork never sends it",
    ),
    (
        "frameDidChange:",
        "view.rs:1073 — the `selector:` ARGUMENT of \
         `addObserver:selector:name:object:`. `WinitView` DECLARES it \
         (view.rs:244) and `winit_seam.rs` censuses that declaration; the \
         notification centre sends it, not the fork",
    ),
    (
        "sendEvent:",
        "app.rs:88 — `class.instance_method(sel!(sendEvent:))`, a method \
         LOOKUP for the override that swizzles it, not a send",
    ),
];

/// `cursor.rs`'s TEN UNDOCUMENTED cursor accessors.
///
/// These are checked differently, and the difference is the fork's own code:
/// `try_cursor_from_selector` asks `-respondsToSelector:` first and falls back
/// to the arrow cursor when the answer is no. A row in [`CLASS_ROWS`] that
/// hard-failed when Apple withdrew one of these would make THIS TEST wrong
/// about a case the shipping code handles correctly.
///
/// So the rule here is conditional — *if* it resolves, it must encode
/// `send_id` — plus a floor on how many resolve today, which is what keeps the
/// table from silently becoming a list of ten selectors that all vanished.
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
        checked >= 183,
        "the census shrank to {checked} rows; it covered 96 when written and \
         179 after W9 phase 2 added `cursor.rs`'s and `view.rs`'s. The floor is \
         the MEASURED count, not a guess: 187 was written here first, from \
         counting the rows by eye, and this assertion is what said 179. Pass 14 \
         raised it to 183 — four sends the fork really makes had no row at all, \
         which is why a floor on the TABLE is not coverage of the SOURCE."
    );
}

/// THE COVERAGE ARM: every selector the fork spells is a row here or a declared
/// non-send.
///
/// # The gap this closes, and why the floor above is not it
///
/// This census was a hand-maintained table checked against the runtime, and
/// nothing checked it against the FORK. Its only coverage-shaped assertion,
/// `checked >= 183`, counts the table's own rows — so a new `sel!` in a ported
/// file with no row here was invisible, and four of them were: `cursor.rs:103`'s
/// ten-argument `-initWithBitmapDataPlanes:…`, `window_delegate.rs:840`'s
/// `-initWithContentRect:…`, and `view.rs:1585` and `event.rs:386`'s two
/// `+[NSEvent …]` factories. All four were MEASURED correct when pass 14 finally
/// asked the runtime — the defect was in the guard, not the code, which is
/// exactly the shape that survives a wave.
///
/// `winit_seam_constants.rs` had already got this right for the seam's 42
/// constants, both ways, in this same wave. This is that rule, one file over.
#[test]
fn every_selector_the_fork_sends_has_a_census_row() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/winit/src/platform_impl/macos");
    let mut seen: Vec<(String, String)> = Vec::new();
    // RECURSIVE. The directory is flat today, and a `read_dir` that assumed so
    // would be a scope that goes stale the day someone adds a subdirectory —
    // the failure mode `objc2_exit_condition.rs` exists to name, where a blind
    // spot widens silently and every test stays green.
    let mut stack = vec![dir.clone()];
    let mut paths = Vec::new();
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("the fork's macOS backend is readable") {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                paths.push(path);
            }
        }
    }
    let mut files = 0_usize;
    for path in paths {
        files += 1;
        let file = path
            .strip_prefix(&dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let src = std::fs::read_to_string(&path).expect("readable");
        for (i, line) in src.lines().enumerate() {
            // Strip `//` comments: this file's own prose names selectors.
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(at) = rest.find("sel!(") {
                rest = &rest[at + "sel!(".len()..];
                // A selector holds no `)`, so the first one closes the call.
                // A multi-line `sel!(\n    foo:bar:\n)` has none on this line,
                // and its selector is on the NEXT line by itself — handled by
                // that line falling through with no `sel!(` and being skipped,
                // so those are collected below from the joined form instead.
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

    // …and BOTH WAYS: an excuse that no longer names a live site must die,
    // or it silently pre-approves a future send of the same selector.
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
    // …and so is the class cluster, in the other direction: the public class
    // really does not implement either initialiser. If Foundation ever folds
    // them onto `NSMutableAttributedString` itself, these belong in the census
    // rather than in the exception list, and this says so.
    for name in ["initWithString:", "initWithAttributedString:"] {
        assert!(
            live_encoding("NSMutableAttributedString", name, false).is_none(),
            "`-[NSMutableAttributedString {name}]` became statically visible; \
             move it into the census instead of excusing it"
        );
    }
}
