// Added by the aterm project in 2026; see the repository NOTICE.
// (This whole file is an aterm addition. It is not upstream winit.)
//
// aterm's seam between this backend's `objc2` BINDINGS and the classes it now
// DECLARES with `aterm_objc`.
//
// # Why a fourth module, and WHAT IT DOES NOT HOLD
//
// `aterm-gui` has an `appkit.rs` doing the same job and it is `pub(crate)` to
// that crate, so a vendored fork cannot reach it. The campaign's own projection
// for the winit port put a ~610-line helper layer here.
//
// IT IS MUCH SMALLER THAN THAT, and the reason is a decision W8 made rather
// than a saving it found. The projected layer was two things welded together:
// typed `objc_msgSend` casts, and AppKit's own constants and globals. The first
// kind names no framework class, no selector and no framework symbol —
// `send_v_usize(recv, sel, a)` knows nothing about AppKit — so it is RUNTIME,
// and it now lives in `aterm_objc::send`, one definition serving both this fork
// and `aterm-gui`. Only the second kind is genuinely a binding, and only the
// second kind is here.
//
// That line is what keeps `aterm-objc`'s "zero framework bindings" rule true
// rather than merely restated, and it is why the fork's ratcheted
// `third_party_loc` pays for ~200 lines of constants instead of ~700 lines of
// constants plus prototypes.
//
// # What this file was, before W8
//
// It said `window_delegate.rs` "keeps every one of its AppKit calls on `objc2`
// (the class pair, the ivars, the `dealloc` and the 23 trampolines are what
// moved), so what it needs is the CROSSING, not the bindings". That was true of
// W3 and W6 and is FALSE NOW: W8 ported all 177 of that file's binding sites,
// which is why the constants, the globals and `obj_of` below exist at all. The
// sentence is kept rather than deleted because a reader who remembers the old
// shape should be told it changed, not left to infer it.
//
// # Safety
//
// `objc2`'s binding types and `aterm_objc`'s declared-class markers are both
// zero-sized types AT an instance address — `objc2` bottoms out in
// `runtime::AnyObject`, which wraps a `[u8; 0]`, and
// `aterm_objc::declare_class!` generates `_opaque: [u8; 0]`. Neither owns any
// of the instance's bytes, so a reference of one kind at a live instance is a
// reference of the other kind at the same instance, and crossing is a
// reinterpretation of the pointer only. What is NOT free, and is the caller's
// obligation at every call, is that the class really is one the target type is
// a correct binding for.

use aterm_objc::Id;
use objc2_foundation::MainThreadMarker;

/// Borrow an `objc2` binding reference at a raw `id`.
///
/// The one direction that has to be `unsafe`: nothing about an `Id` says which
/// class it addresses, and picking the wrong `T` is the same defect as an
/// unchecked cast in Objective-C.
///
/// # Safety
///
/// `id` must be non-null and address a live instance of a class that `T` is a
/// correct `objc2` binding for — including `T = ProtocolObject<dyn P>`, where
/// the obligation is that the class actually conforms to `P` (for the classes
/// this backend declares, `class_addProtocol` is what makes that true, and
/// `aterm_objc::declare_class!`'s `protocols:` list is where it happens).
#[must_use]
pub(super) unsafe fn objc2_ref<'a, T>(id: Id) -> &'a T {
    debug_assert!(!id.is_null(), "objc2_ref of nil");
    // SAFETY: the caller guarantees `id` addresses a live instance of `T`'s
    // class; `T` is a zero-sized opaque binding marker, so the reference
    // borrows none of the instance's own bytes.
    unsafe { &*id.as_ptr().cast_const().cast::<T>() }
}

/// Re-badge `objc2`'s main-thread marker as `aterm_objc`'s.
///
/// # Why this is not a `From` impl, and why it re-asks
///
/// The two types mean the same thing — "the caller has established that this is
/// the process main thread" — but they are minted by different crates and only
/// one of them can gate `alloc_init`. `MainThreadMarker` is `!Send` and is
/// itself only obtainable from `MainThreadMarker::new()`'s
/// `is_main_thread()` check or from an `unsafe` constructor, so holding one IS
/// the proof [`aterm_objc::MainThread`] wants.
///
/// It re-asks anyway, through the checked constructor, and `expect`s. The cost
/// is one `+[NSThread isMainThread]` per WINDOW CREATION — not per message, not
/// per frame — and in exchange the conversion contains no `unsafe` token and no
/// "trust me" comment for a future reader to have to re-derive. If the answer
/// were ever `None` while an `objc2` marker was in hand, one of the two crates
/// would be wrong about the thread and a panic here is strictly better than
/// building an AppKit object on the strength of it.
#[must_use]
pub(super) fn witness(_mtm: MainThreadMarker) -> aterm_objc::MainThread {
    aterm_objc::MainThread::new().expect(
        "holding an objc2 MainThreadMarker while +[NSThread isMainThread] says otherwise",
    )
}

/// Re-badge `aterm_objc`'s main-thread witness as `objc2`'s marker — the
/// INVERSE of [`witness`], and the direction W9 found it needed.
///
/// # Why the pair has to be bidirectional, which is the whole finding
///
/// `MainThreadMarker` is the single most widely shared `objc2` name left in
/// this backend: twelve of the eighteen files that still import the crate
/// mention it. The campaign priced substituting it as the cheapest large move
/// BECAUSE it is a zero-sized marker rather than a binding — nothing about it
/// addresses an object, so swapping it for [`aterm_objc::MainThread`] moves no
/// message send at all.
///
/// With only [`witness`] the substitution could not START, and the reason is
/// worth stating because it is a general shape and not a quirk of this type. A
/// one-way conversion forces the port to proceed ROOT-FIRST: a file may only
/// flip once every marker it hands onward has already flipped, so nothing can
/// move until `NSApplication::sharedApplication`, `NSScreen::screens`,
/// `NSMenu::new` and `MainThreadBound` — the framework bindings that CONSUME a
/// marker — are ported first. Those are the expensive rows.
///
/// This function inverts the order. With both directions available the port
/// proceeds LEAF-FIRST: a file whose marker is consumed only by first-party
/// code flips today and takes its own signature with it, and each file that is
/// still pinned by a framework binding re-derives a marker at that one call
/// site. `window_delegate.rs` — the largest file in the endgame, and the one
/// W8 emptied of all 177 send sites without moving the file count — is freed by
/// exactly this and by nothing else.
///
/// # Why it is sound in this direction too
///
/// The two types answer the same question with the same primitive. objc2's
/// `is_main_thread()` calls `pthread_main_np()` and says in its own comment
/// that this is what `+[NSThread isMainThread]` does under the hood;
/// [`aterm_objc::MainThread::new`] sends `+[NSThread isMainThread]`. Both
/// markers are `!Send` and `!Sync`, so holding either one means THIS thread
/// answered yes.
///
/// It re-asks anyway, through the checked constructor, exactly as [`witness`]
/// does and for the same reason: the conversion then contains no `unsafe` token
/// and no "trust me" comment. If the answer were ever `None` while an
/// `aterm_objc` witness was in hand, one of the two crates would be wrong about
/// the thread, and a panic here is strictly better than reaching AppKit on the
/// strength of it.
#[must_use]
pub(super) fn marker(_w: aterm_objc::MainThread) -> MainThreadMarker {
    MainThreadMarker::new().expect(
        "holding an aterm_objc MainThread while objc2's pthread_main_np() says otherwise",
    )
}

// ---------------------------------------------------------------------------
// GEOMETRY: NONE. The section that was here is DELETED, and the arithmetic of
// its life is the note.
//
// W8 projected a conversion per geometric shape in each direction and wrote
// six. FIVE WERE DEAD ON ARRIVAL and the compiler said so the same day; the
// sixth, `cg_rect`, survived with exactly one caller — `view.rs`'s
// `firstRectForCharacterRange:actualRange:` — and its own doc comment said as
// much. W9 phase 2 ports that file, so the last caller is gone and the section
// with it. Six projected, zero surviving, over two waves.
//
// `id_of` below went the same way in the same commit and is worth the second
// example, because its shape was different: it was not over-projected, it had
// SEVEN live callers, and every one of them was in `view.rs` handing an objc2
// `&NSEvent` to `event.rs`, which phase 1 had already ported to take a raw
// `id`. A crossing is dead when EITHER side stops needing it, and a count of
// call sites says nothing about which.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// THE REVERSE CROSSING. Added for `window_delegate.rs` (W8).
// ---------------------------------------------------------------------------

/// An owning [`aterm_objc::Obj`] at an `objc2` binding reference, +1.
///
/// # Why this direction exists again
///
/// `aterm-gui` had a function like this (`id_of`) and W7 DELETED it, because
/// once `toolbar.rs`'s bindings were ported nothing crossed that way any more.
/// The fork is not in that position and will not be for several waves: this
/// backend is ported FILE BY FILE, and a ported file's unported neighbours hand
/// it `objc2` handles across module boundaries — `monitor.rs`'s
/// `ns_screen() -> Option<Retained<NSScreen>>` and `cursor.rs`'s
/// `default_cursor() -> Retained<NSCursor>` are the live examples. Re-badging
/// one of those at the boundary is what lets the RECEIVING file send through
/// the first-party layer without its unported neighbour changing at all.
///
/// It retains, and that is the whole design. `Retained<T>`'s +1 belongs to the
/// caller's binding handle and will be released when that handle drops; the
/// `Obj` returned here is a SECOND +1 with its own lifetime, so the two can be
/// dropped in either order. Borrowing the raw pointer instead would tie an
/// `aterm_objc` value's validity to an `objc2` value's scope through a link the
/// compiler cannot see — the exact defect the tenth pass found in
/// `app_introspect.rs` and that `appkit::objc2_ref` was re-signatured to fix.
#[must_use]
pub(super) fn obj_of<T>(r: &T) -> aterm_objc::Obj {
    let id = Id::from_ptr((r as *const T).cast_mut().cast());
    // SAFETY: `r` borrows a live Objective-C instance (it is an `objc2` binding
    // reference, which cannot exist otherwise), so `objc_retain` on its address
    // is valid and yields a +1 this `Obj` owns.
    unsafe { aterm_objc::Obj::retain(id) }.expect("retaining a live objc2 binding reference")
}

// ---------------------------------------------------------------------------
// STRINGS.
// ---------------------------------------------------------------------------

/// An `NSString` for `s`, +1, or `None` if Foundation refused it.
#[must_use]
pub(super) fn nsstring(s: &str) -> Option<aterm_objc::Obj> {
    aterm_objc::ns_string(s)
}

/// The Rust `String` inside an `NSString`, or `String::new()` for nil.
///
/// # Safety
/// `s` must be nil or a live `NSString`.
#[must_use]
pub(super) unsafe fn nsstring_to_rust(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees `s` is a live `NSString`.
    unsafe { aterm_objc::ns_string_to_rust(s) }
}

// ---------------------------------------------------------------------------
// AppKit and Foundation CONSTANTS, at the SDK's own values.
//
// # Every value below is `_Static_assert`ed against the SDK on BOTH arches
//
// `aterm-gui`'s `appkit::consts` explains why these are `const`s and not extern
// statics: each is declared in a HEADER, as a `static const` with internal
// linkage or as an enumerator, so there is no symbol to bind. That module also
// records what happens when a constant is read once and trusted —
// `NS_TEXT_ALIGNMENT_CENTER` shipped as `2` (which is RIGHT alignment) because
// the `#if TARGET_ABI_USES_IOS_VALUES` guarding it was read as "iOS vs macOS"
// when on Apple Silicon it reduces to `!TARGET_CPU_X86_64`. It compiled, every
// test passed, every encoding read back correct, and only an A/B PIXEL capture
// caught it.
//
// So none of these is read once. Every one is a compile-time value, which means
// COMPILING a `_Static_assert` for an arch is the measurement and no binary for
// that arch has to run — which matters because this box cannot execute x86_64
// and `gate cells` has no `x86_64-apple-darwin` cell:
//
//   $ cat > /tmp/c.m <<'EOF'
//   #import <Cocoa/Cocoa.h>
//   _Static_assert(NSWindowStyleMaskResizable == 1 << 3, "");
//   … one line per constant below …
//   int main(void){return 0;}
//   EOF
//   $ cc -arch arm64  -fsyntax-only /tmp/c.m   # passes
//   $ cc -arch x86_64 -fsyntax-only /tmp/c.m   # passes
//
// Both arms pass at every value written here, and the check is LOAD-BEARING
// rather than vacuous: changing the `1 << 3` above to `1 << 4` fails to compile
// with "static assertion failed" on both. `sizeof` rows are asserted the same
// way, because two prototypes in `aterm_objc::send` depend on them (`NSRect` is
// 32 bytes, `NSRange` is 16, and `-keyCode` really is 2).
//
// NOT ONE OF THESE VALUES DIFFERS BETWEEN THE TWO ARCHES. That is the measured
// result, not an assumption — it is exactly what the tab strip's alignment
// constant did NOT satisfy, and the only reason it can be stated here is that
// both arms were compiled.
// ---------------------------------------------------------------------------
pub(super) mod consts {
    // ---- NSWindow.h, NSWindowStyleMask (NSUInteger) ----
    pub(crate) const NS_WINDOW_STYLE_MASK_BORDERLESS: usize = 0;
    pub(crate) const NS_WINDOW_STYLE_MASK_TITLED: usize = 1 << 0;
    pub(crate) const NS_WINDOW_STYLE_MASK_CLOSABLE: usize = 1 << 1;
    pub(crate) const NS_WINDOW_STYLE_MASK_MINIATURIZABLE: usize = 1 << 2;
    pub(crate) const NS_WINDOW_STYLE_MASK_RESIZABLE: usize = 1 << 3;
    pub(crate) const NS_WINDOW_STYLE_MASK_FULL_SIZE_CONTENT_VIEW: usize = 1 << 15;

    // ---- NSApplication.h, NSApplicationPresentationOptions (NSUInteger) ----
    pub(crate) const NS_APPLICATION_PRESENTATION_AUTO_HIDE_DOCK: usize = 1 << 0;
    pub(crate) const NS_APPLICATION_PRESENTATION_HIDE_DOCK: usize = 1 << 1;
    pub(crate) const NS_APPLICATION_PRESENTATION_AUTO_HIDE_MENU_BAR: usize = 1 << 2;
    pub(crate) const NS_APPLICATION_PRESENTATION_HIDE_MENU_BAR: usize = 1 << 3;
    pub(crate) const NS_APPLICATION_PRESENTATION_FULL_SCREEN: usize = 1 << 10;

    // ---- NSWindow.h, NSWindowButton (NSUInteger) ----
    pub(crate) const NS_WINDOW_CLOSE_BUTTON: usize = 0;
    pub(crate) const NS_WINDOW_MINIATURIZE_BUTTON: usize = 1;
    pub(crate) const NS_WINDOW_ZOOM_BUTTON: usize = 2;
    /// `NSWindow.h:1062` — deprecated since 10.12 and documented to answer nil
    /// for every window since; the fork asks for it anyway, exactly as it did
    /// through `objc2-app-kit`, so that hiding the titlebar buttons keeps
    /// covering the same four cases it used to.
    pub(crate) const NS_WINDOW_FULL_SCREEN_BUTTON: usize = 7;

    // ---- NSWindow.h, NSWindowSharingType (NSUInteger) ----
    pub(crate) const NS_WINDOW_SHARING_NONE: usize = 0;
    pub(crate) const NS_WINDOW_SHARING_READ_ONLY: usize = 1;

    // ---- NSWindow.h, NSWindowTitleVisibility (NSInteger) ----
    pub(crate) const NS_WINDOW_TITLE_HIDDEN: isize = 1;

    // ---- NSWindow.h, NSWindowTabbingMode (NSInteger) ----
    pub(crate) const NS_WINDOW_TABBING_MODE_PREFERRED: isize = 1;

    // ---- NSGraphics.h, NSWindowOrderingMode (NSInteger) ----
    pub(crate) const NS_WINDOW_ABOVE: isize = 1;

    // ---- NSWindow.h, NSWindowOcclusionState (NSUInteger) ----
    pub(crate) const NS_WINDOW_OCCLUSION_STATE_VISIBLE: usize = 1 << 1;

    // ---- NSApplication.h, NSRequestUserAttentionType (NSUInteger) ----
    //
    // UNSIGNED, and the type is the whole note on `send_isize_usize`: these were
    // written `isize` from the header's "NSInteger-looking" neighbours and
    // `method_getTypeEncoding` answered `q24@0:8Q16` — a SIGNED return over an
    // UNSIGNED argument. Nothing at runtime could have told the difference.
    pub(crate) const NS_CRITICAL_REQUEST: usize = 0;
    pub(crate) const NS_INFORMATIONAL_REQUEST: usize = 10;

    // ---- NSGraphics.h, NSBackingStoreType (NSUInteger) ----
    pub(crate) const NS_BACKING_STORE_BUFFERED: usize = 2;

    // ---- NSDragging.h, NSDragOperation (NSUInteger) ----
    pub(crate) const NS_DRAG_OPERATION_NONE: usize = 0;
    pub(crate) const NS_DRAG_OPERATION_COPY: usize = 1;

    // ---- NSKeyValueObserving.h, NSKeyValueObservingOptions (NSUInteger) ----
    pub(crate) const NS_KEY_VALUE_OBSERVING_OPTION_NEW: usize = 0x01;
    pub(crate) const NS_KEY_VALUE_OBSERVING_OPTION_OLD: usize = 0x02;

    /// `NSApplication.h` — `NSAppKitVersionNumber10_12 = 1504`. Compared
    /// against the LIVE `NSAppKitVersionNumber` (an extern `double`, bound
    /// below), which reads 2685.6 on this box.
    pub(crate) const NS_APPKIT_VERSION_NUMBER_10_12: f64 = 1504.0;

    // ---- NSEvent.h, NSEventModifierFlags (NSUInteger) ----
    //
    // W10, for `event.rs`. `objc2-app-kit` generated these as a `bitflags`
    // type; here they are the header's values, asserted on both arches like
    // every other row above. The four the fork reads are the DEVICE-INDEPENDENT
    // ones — the left/right device masks `event.rs` also tests come from
    // IOLLEvent.h, are not AppKit constants at all, and stay in that file
    // beside the comment naming their source.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_SHIFT: usize = 1 << 17;
    pub(crate) const NS_EVENT_MODIFIER_FLAG_CONTROL: usize = 1 << 18;
    pub(crate) const NS_EVENT_MODIFIER_FLAG_OPTION: usize = 1 << 19;
    pub(crate) const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

    // ---- NSEvent.h, NSEventType (NSUInteger) ----
    pub(crate) const NS_EVENT_TYPE_APPLICATION_DEFINED: usize = 15;

    // ---- NSEvent.h, NSEventSubtype (short) ----
    //
    // SIGNED and NARROW, and both halves matter. `NSEventSubtype` is the one
    // enumeration in this module whose underlying type is neither `NSInteger`
    // nor `NSUInteger`: `-subtype` and the `subtype:` argument of
    // `+otherEventWithType:…` are bare `short`, which the runtime spells `s`
    // (the unsigned twin would be `S`). The value being 0 is exactly why no
    // test could have caught the wrong width or sign at runtime.
    pub(crate) const NS_EVENT_SUBTYPE_WINDOW_EXPOSED: i16 = 0;

    // ---- NSEvent.h, NSEventPhase (NSUInteger) ----
    //
    // W9 phase 2, for `view.rs`'s four gesture rows. FIVE of the seven
    // enumerators are here, and the two that are missing are missing on
    // purpose: `NSEventPhaseNone` (0) and `NSEventPhaseStationary` (1 << 1) are
    // never compared against by the fork — every `match` on a phase ends in a
    // wildcard arm that covers them — and the rule this module has followed
    // since W8 is that a constant nothing reads is a claim nothing checks.
    //
    // `objc2-app-kit` generated these as a `bitflags` type and the fork
    // compared with `NSEventPhase::Began` and friends. They are a BITMASK, not
    // an enumeration of adjacent integers: `Ended` is `1 << 3`, so a reader who
    // assumed dense numbering would write 3 and match `Changed | Stationary`.
    pub(crate) const NS_EVENT_PHASE_BEGAN: usize = 1 << 0;
    pub(crate) const NS_EVENT_PHASE_CHANGED: usize = 1 << 2;
    pub(crate) const NS_EVENT_PHASE_ENDED: usize = 1 << 3;
    pub(crate) const NS_EVENT_PHASE_CANCELLED: usize = 1 << 4;
    pub(crate) const NS_EVENT_PHASE_MAY_BEGIN: usize = 1 << 5;

    /// `NSObjCRuntime.h` — `NSNotFound = NSIntegerMax`, the "no marked range"
    /// and "no selection" answer `view.rs` returns from `-markedRange` and
    /// `-selectedRange`.
    ///
    /// It is an `NSInteger` in the header and is written into the `location`
    /// field of an `NSRange`, which is an `NSUInteger`. That reinterpretation
    /// is what `NSNotFound as NSUInteger` did before the port and what the
    /// `usize` type here does now; the value is `NSIntegerMax`, so it is
    /// positive in both readings and no bit changes.
    pub(crate) const NS_NOT_FOUND: usize = 0x7fff_ffff_ffff_ffff;

    /// `NSString.h` — `NSUTF8StringEncoding`, the argument to
    /// `-lengthOfBytesUsingEncoding:`.
    ///
    /// This constant exists because of the distinction on
    /// [`aterm_objc::send::send_usize_usize`]: `objc2-foundation`'s
    /// `NSString::len()` is `-lengthOfBytesUsingEncoding:` with THIS value and
    /// its `len_utf16()` is `-length`, and `view.rs` calls both on the IME
    /// pre-edit path three lines apart.
    pub(crate) const NS_UTF8_STRING_ENCODING: usize = 4;
}

// ---------------------------------------------------------------------------
// The FRAMEWORK GLOBALS — the exception the `consts` note names.
//
// These seven are real exported symbols rather than header values, and the
// difference is measured rather than assumed: each is declared
// `APPKIT_EXTERN`/`FOUNDATION_EXPORT` in its header, and a linked probe reads
// every one back non-nil on this box —
//
//   NSFilenamesPboardType            = "NSFilenamesPboardType"
//   NSKeyValueChangeNewKey           = "new"
//   NSKeyValueChangeOldKey           = "old"
//   NSAppearanceNameAqua             = "NSAppearanceNameAqua"
//   NSViewFrameDidChangeNotification = "NSViewFrameDidChangeNotification"
//   NSDeviceRGBColorSpace            = "NSDeviceRGBColorSpace"
//   NSAppKitVersionNumber            = 2685.6
//
// THIS NOTE SAID "six" AND DECLARED FIVE, for the whole of W8. The list above
// already carried `NSViewFrameDidChangeNotification` — probed, measured, and
// written down — while the `extern` blocks below bound only the five
// `window_delegate.rs` reads, because the sixth belonged to `view.rs` and that
// file had not been ported. It is the mirror image of the mistake the `consts`
// note ends on: that one refuses to leave a constant nothing reads, and this
// one left a MEASUREMENT nothing declared, so the count in the prose was a
// claim no `extern` line answered for. W9 phase 2 ports `view.rs` and
// `cursor.rs`, which is what makes the sixth and seventh real.
//
// Writing any of them as a `const` would invent a value AppKit does not
// recognise — `NSKeyValueChangeNewKey` is the one that makes this concrete: its
// STRING is `"new"`, which no reader would have guessed from the symbol name.
//
// They are VARIABLES, so each binding is the address of a live framework global
// and each read is one dereference, not a call.
// ---------------------------------------------------------------------------
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    /// `NSApplication.h` — the running AppKit's version, as a `double`.
    #[link_name = "NSAppKitVersionNumber"]
    pub(super) static NS_APPKIT_VERSION_NUMBER: f64;
    /// `NSPasteboard.h:505` — the legacy filenames pasteboard type. Deprecated
    /// since 10.14 in favour of `NSPasteboardTypeFileURL`; the fork reads the
    /// same one it read through `objc2-app-kit`, so that drag-and-drop keeps
    /// accepting exactly what it accepted before.
    #[link_name = "NSFilenamesPboardType"]
    pub(super) static NS_FILENAMES_PBOARD_TYPE: Id;
    /// `NSAppearance.h:63` — the light system appearance's name.
    #[link_name = "NSAppearanceNameAqua"]
    pub(super) static NS_APPEARANCE_NAME_AQUA: Id;
    /// `NSView.h` — the notification `view.rs` observes to learn its frame
    /// changed. Its string is `"NSViewFrameDidChangeNotification"`, which is
    /// the one case in this block where the obvious guess would have been
    /// right — and `NSKeyValueChangeNewKey` two declarations below, whose
    /// string is `"new"`, is why guessing is not the rule.
    #[link_name = "NSViewFrameDidChangeNotification"]
    pub(super) static NS_VIEW_FRAME_DID_CHANGE_NOTIFICATION: Id;
    /// `NSGraphics.h` — the `NSColorSpaceName` `cursor.rs` hands
    /// `-initWithBitmapDataPlanes:…colorSpaceName:…`. Its string is
    /// `"NSDeviceRGBColorSpace"`.
    #[link_name = "NSDeviceRGBColorSpace"]
    pub(super) static NS_DEVICE_RGB_COLOR_SPACE: Id;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    /// `NSKeyValueObserving.h` — the change dictionary's new-value key. Its
    /// string is `"new"`.
    #[link_name = "NSKeyValueChangeNewKey"]
    pub(super) static NS_KEY_VALUE_CHANGE_NEW_KEY: Id;
    /// `NSKeyValueObserving.h` — the change dictionary's old-value key. Its
    /// string is `"old"`.
    #[link_name = "NSKeyValueChangeOldKey"]
    pub(super) static NS_KEY_VALUE_CHANGE_OLD_KEY: Id;
}
