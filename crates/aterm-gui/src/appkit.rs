// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's own AppKit/Foundation layer over [`aterm_objc`]. `aterm-objc` is the runtime
//! (typed sends, ownership, class creation, the block ABI) and binds no framework; this module
//! holds what names AppKit ([`consts`], [`existing_application`], the `NSAppearanceName`
//! globals, [`beep`]) and re-exports [`MainThread`] and the typed send shapes. No file in this
//! crate uses `objc2`: the manifest depends on no objc2-family crate.
//!
//! # Ownership
//!
//! The send helpers return a raw [`Id`]. They cannot know whether a selector returns +1 or
//! +0, so the caller decides, as in Objective-C: an `alloc`/`new`/`copy`/`mutableCopy` result
//! goes into [`aterm_objc::Obj`] or [`aterm_objc::Retained`]; anything else is borrowed and
//! lives until the enclosing [`aterm_objc::autoreleasepool`] pops.
//!
//! # Safety
//!
//! Every send helper is `unsafe` for one reason: the caller asserts that the selector really
//! has the C prototype named in the helper's name and that `recv` is a live receiver that
//! responds to it. The wrong helper is the same defect as the wrong hand-written cast:
//! corrupted registers on both Apple ABIs.

use aterm_objc::Id;

/// A witness that the current thread is the process main thread: [`aterm_objc::MainThread`]
/// under this module's name. It lives in `aterm-objc` because `alloc_init`/`alloc_ivars`
/// take it, so a declared class cannot be instantiated off the main thread without one.
pub(crate) use aterm_objc::MainThread;

/// The instantiation witness for a unit test, which never runs on the main thread.
///
/// libtest runs every test on a worker (`pthread_main_np()` is 0 even under
/// `--test-threads=1`), so [`MainThread::new`] answers `None` there. The probe classes tests
/// instantiate are `NSObject`/`NSView` subclasses that touch no AppKit state and whose
/// `Ivars` are born and dropped on the same worker, which meets `new_unchecked`'s obligation.
#[cfg(test)]
pub(crate) fn test_witness() -> MainThread {
    // SAFETY: see this function's doc comment — no main-thread affinity, and
    // the ivars never leave the worker that created them.
    unsafe { MainThread::new_unchecked() }
}

/// The typed [`aterm_objc::msg`] casts, one `unsafe fn` per C prototype rather than per
/// selector, re-exported so call sites say `appkit::send_v_id(recv, sel!(setMenu:), menu)`.
///
/// They are the module note's second bullet: `send_v_id(recv, sel!(setMenu:),
/// menu)` written once instead of once per call site, 35 shapes serving this
/// crate's 206 distinct selectors. Every call site in `aterm-gui` —
/// `toolbar.rs`'s 268 among them — is unchanged by the move and still says
/// `appkit::send_v_id`.
///
/// WHY THEY LEFT: `crates/aterm-window`'s `window_delegate.rs` and `view.rs` need the
/// same shapes and CANNOT import this module, which is `pub(crate)` to
/// `aterm-gui`. The two honest options were a second copy inside the fork or
/// one copy somewhere both can reach. A second copy would have added ~500
/// lines of first-party ABI plumbing to the fork's third-party line count —
/// the surface the campaign exists to SHRINK — and left two copies of a
/// safety-critical layer free to drift apart.
///
/// WHY `aterm-objc` IS WHERE THEY WENT, and not a new binding crate: not one of
/// these functions names a framework class, a selector, or a framework symbol.
/// `send_v_usize(recv, sel, a)` knows nothing about AppKit; it is a cast of
/// [`aterm_objc::msg`], which is that crate's own stated rule ("every send
/// therefore casts `msg` to the EXACT prototype of the selector"). The things
/// that DO name AppKit — [`consts`], the `NSAppearanceName*` globals,
/// [`appearance_name`] — stayed here, and the fork's equivalents stayed in the
/// fork. That line is what keeps `aterm-objc`'s "zero framework bindings" rule
/// true rather than merely restated.
///
/// The glob is deliberate: it re-exports the shapes this crate uses and the
/// ones only the fork uses, without a name list that would go stale on one side
/// of the tree every time the other side ports a file.
pub(crate) use aterm_objc::send::*;

/// An `NSString` for `s`, +1, or `None` if Foundation refused it ([`aterm_objc::ns_string`]).
#[must_use]
pub(crate) fn nsstring(s: &str) -> Option<aterm_objc::Obj> {
    aterm_objc::ns_string(s)
}

/// The Rust `String` inside an `NSString`, or `String::new()` for nil.
///
/// # Safety
/// `s` must be nil or a live `NSString`.
#[must_use]
pub(crate) unsafe fn nsstring_to_rust(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees `s` is a live `NSString`.
    unsafe { aterm_objc::ns_string_to_rust(s) }
}

/// AppKit, Foundation and Core Graphics constants, at the SDK's own values.
///
/// Every one is a `const`: each is declared in an SDK header as a `static const`, an
/// enumerator or a `#define`, so there is no symbol to link (an `extern` static fails with
/// `Undefined symbols: "_NSVariableStatusItemLength"`). The `NSAppearanceName*` pair are real
/// symbols, bound below and served by [`appearance_name`].
///
/// Each constant cites the SDK header and line it was read from.
/// `crates/aterm-objc/tests/gui_appkit_constants.rs` `_Static_assert`s every one against the
/// SDK on both arches and needs a row for each.
pub(crate) mod consts {
    /// `NSStatusBar.h:20` — `static const CGFloat NSVariableStatusItemLength = -1.0;`
    pub(crate) const NS_VARIABLE_STATUS_ITEM_LENGTH: f64 = -1.0;

    /// `NSEvent.h:169` — `NSEventModifierFlagShift = 1 << 17`.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_SHIFT: usize = 1 << 17;

    /// `NSEvent.h:170` — `NSEventModifierFlagControl = 1 << 18`.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_CONTROL: usize = 1 << 18;

    /// `NSEvent.h:172` — `NSEventModifierFlagCommand = 1 << 20`.
    pub(crate) const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

    /// `NSWindow.h:71` — `static const NSModalResponse NSModalResponseOK = 1;`
    pub(crate) const NS_MODAL_RESPONSE_OK: isize = 1;

    // ---- the tab strip ----

    /// `NSBezierPath.h:20` — `NSLineCapStyleRound = 1` (`NSUInteger`).
    pub(crate) const NS_LINE_CAP_STYLE_ROUND: usize = 1;

    /// `NSText.h:48` — `NSTextAlignmentLeft = 0` (`NSInteger`).
    pub(crate) const NS_TEXT_ALIGNMENT_LEFT: isize = 0;

    /// `NSText.h:50/54` — `NSTextAlignmentCenter`: 1 on arm64 macOS, 2 on the x86_64 slice.
    ///
    /// so on macOS it reduces to `!TARGET_CPU_X86_64` — **true on Apple
    /// Silicon**. Measured with clang on this box: `TARGET_ABI_USES_IOS_VALUES
    /// = 1`, `NSTextAlignmentCenter = 1`, `NSTextAlignmentRight = 2`. The `2`
    /// was therefore RIGHT alignment, and every tab label in the strip is
    /// `Center`: an A/B capture of the live titlebar showed the labels of the
    /// two unselected chips displaced +47px inside cells whose pill, traffic
    /// lights and "+" had not moved a pixel. Nothing else could see it — it
    /// compiled, every test passed, and the encoding was correct.
    ///
    /// Two readings of a value are not a measurement when the second is read as
    /// agreement rather than evaluated. [`super::consts_tests`] now evaluates
    /// `objc2-app-kit`'s own constants and compares, so this cannot drift again
    /// while that crate is still in the tree.
    ///
    /// BOTH ARMS ARE MEASURED, including the one this box cannot execute. These
    /// are compile-time constants, so COMPILING a `_Static_assert` for an arch
    /// is the measurement and no x86_64 binary has to run:
    ///
    /// ```text
    /// $ cat > /tmp/ta.m <<'EOF'
    /// #import <Cocoa/Cocoa.h>
    /// #include <TargetConditionals.h>
    /// #if TARGET_CPU_X86_64
    /// _Static_assert(TARGET_ABI_USES_IOS_VALUES == 0, "");
    /// _Static_assert(NSTextAlignmentCenter == 2, "");
    /// #else
    /// _Static_assert(TARGET_ABI_USES_IOS_VALUES == 1, "");
    /// _Static_assert(NSTextAlignmentCenter == 1, "");
    /// #endif
    /// int main(void){return 0;}
    /// EOF
    /// $ cc -arch arm64  -fsyntax-only /tmp/ta.m   # passes
    /// $ cc -arch x86_64 -fsyntax-only /tmp/ta.m   # passes
    /// ```
    ///
    /// Inverting either assertion fails to compile, so the check is
    /// load-bearing rather than vacuous. No check compiles the
    /// `x86_64-apple-darwin` cell, so this is the only thing standing behind
    /// the `= 2` arm.
    #[cfg(target_arch = "x86_64")]
    pub(crate) const NS_TEXT_ALIGNMENT_CENTER: isize = 2;
    /// The arm64 twin: `TARGET_ABI_USES_IOS_VALUES` is true here.
    #[cfg(not(target_arch = "x86_64"))]
    pub(crate) const NS_TEXT_ALIGNMENT_CENTER: isize = 1;

    /// `NSCell.h:49` — `NSNoImage = 0` (`NSCellImagePosition`, `NSUInteger`).
    pub(crate) const NS_NO_IMAGE: usize = 0;

    /// `NSTrackingArea.h:19,29,35` — `NSTrackingMouseEnteredAndExited` (0x01)
    /// `| NSTrackingActiveAlways` (0x80) `| NSTrackingInVisibleRect` (0x200),
    /// the one combination the strip installs: hover in/out, regardless of app
    /// activation, following the view's visible rect across resizes.
    pub(crate) const NS_TRACKING_HOVER_IN_VISIBLE_RECT: usize = 0x01 | 0x80 | 0x200;

    /// `NSView.h:35,40` — `NSViewMinXMargin` (1) `| NSViewMaxYMargin` (32):
    /// right-anchored, top-anchored. The "+" button's mask between re-layouts.
    pub(crate) const NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN: usize = 1 | 32;

    /// `NSView.h:36` — `NSViewWidthSizable = 2`. The strip container's mask.
    pub(crate) const NS_VIEW_WIDTH_SIZABLE: usize = 2;

    /// `NSWindow.h:215-217` — `NSWindowCloseButton` (0),
    /// `NSWindowMiniaturizeButton` (1), `NSWindowZoomButton` (2), in the order
    /// `strip_metrics` measures them. `NSUInteger`, so the send is
    /// [`super::send_id_usize`].
    pub(crate) const NS_WINDOW_CLOSE_BUTTON: usize = 0;
    pub(crate) const NS_WINDOW_MINIATURIZE_BUTTON: usize = 1;
    pub(crate) const NS_WINDOW_ZOOM_BUTTON: usize = 2;

    /// `NSGraphics.h:104` — `NSWindowAbove = 1` (`NSInteger`).
    pub(crate) const NS_WINDOW_ABOVE: isize = 1;

    /// `NSToolbar.h:26` — `NSToolbarDisplayModeIconOnly = 2` (`NSUInteger`).
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY: usize = 2;

    /// `NSWindow.h:248` — `NSWindowToolbarStyleUnifiedCompact = 4` (`NSInteger`): the single
    /// compact chrome row the strip lives in.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT: isize = 4;

    /// `NSWindow.h:231` — `NSWindowTitleHidden = 1` (`NSInteger`).
    pub(crate) const NS_WINDOW_TITLE_HIDDEN: isize = 1;

    /// `NSParagraphStyle.h:26,30` — `NSLineBreakByWordWrapping = 0` and three implicit
    /// enumerators after it, so `NSLineBreakByTruncatingTail = 4` (`NSUInteger`): a chip title
    /// too long for its chip ends in an ellipsis.
    pub(crate) const NS_LINE_BREAK_BY_TRUNCATING_TAIL: usize = 4;

    // ---- the modal-alert subsystem ----

    /// `NSEvent.h:103` — `NSEventMaskKeyDown = 1ULL << NSEventTypeKeyDown`, with
    /// `NSEventTypeKeyDown = 10` (`NSEvent.h:34`). An `NSEventMask` (`unsigned long long`),
    /// passed as the `NSUInteger` `+addLocalMonitorForEventsMatchingMask:handler:` takes;
    /// both are 64-bit on every Apple target. The one mask `alert_keys` installs.
    pub(crate) const NS_EVENT_MASK_KEY_DOWN: usize = 1 << 10;

    /// `NSAlert.h:50` — `static const NSModalResponse NSAlertFirstButtonReturn = 1000;`. The
    /// first button added is the default: `menu::confirm`'s proceed and the paste sheet's
    /// "Paste".
    pub(crate) const NS_ALERT_FIRST_BUTTON_RETURN: isize = 1000;
    /// `NSAlertSecondButtonReturn`: the second button added, the destructive one of an
    /// owner-gesture alert whose first (default) button is Cancel.
    pub(crate) const NS_ALERT_SECOND_BUTTON_RETURN: isize = 1001;

    // ---- the `chrome` introspection reader ----
    //
    // `NSWindowToolbarStyle` (`NSWindow.h:243-249`) and `NSToolbarDisplayMode`
    // (`NSToolbar.h:23-28`) are implicit enumerations, so each enumerator is its ordinal.
    // `app_introspect::read_native_chrome` turns them into words.

    /// `NSWindow.h:244` — `NSWindowToolbarStyleAutomatic` (`NSInteger`).
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_AUTOMATIC: isize = 0;
    /// `NSWindow.h:245` — `NSWindowToolbarStyleExpanded`.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_EXPANDED: isize = 1;
    /// `NSWindow.h:246` — `NSWindowToolbarStylePreference`.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_PREFERENCE: isize = 2;
    /// `NSWindow.h:247` — `NSWindowToolbarStyleUnified`.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_UNIFIED: isize = 3;

    /// `NSToolbar.h:24` — `NSToolbarDisplayModeDefault` (`NSUInteger`).
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_DEFAULT: usize = 0;
    /// `NSToolbar.h:25` — `NSToolbarDisplayModeIconAndLabel`.
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_ICON_AND_LABEL: usize = 1;
    /// `NSToolbar.h:27` — `NSToolbarDisplayModeLabelOnly`.
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_LABEL_ONLY: usize = 3;

    // ---- the titlebar snapshot ----

    /// `NSBitmapImageRep.h:37` — `NSBitmapImageFileTypePNG` (`NSUInteger`), the
    /// fifth enumerator of an implicit enum, so 4.
    pub(crate) const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;

    /// `NSGraphics.h:129` — `NSColorRenderingIntentPerceptual` (`NSInteger`), the fourth
    /// enumerator of an implicit enum, so 3.
    pub(crate) const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;

    // ---- the App Nap opt-out ----

    /// `NSProcessInfo.h:384` — `NSActivityUserInitiatedAllowingIdleSystemSleep =
    /// (NSActivityUserInitiated & ~NSActivityIdleSystemSleepDisabled)`, where
    /// `NSActivityUserInitiated = (0x00FFFFFFULL | NSActivityIdleSystemSleepDisabled)` (`:382`)
    /// and `NSActivityIdleSystemSleepDisabled = (1ULL << 20)` (`:353`). Bit 20 lies inside
    /// `0x00FF_FFFF`, so the value is `0x00EF_FFFF`: every user-initiated flag (App Nap off)
    /// minus the one that stops idle sleep.
    ///
    /// Spelled folded, never as a Rust `!` expression: the SDK test reads the literal as C,
    /// where `!` is logical not. `NSActivityOptions` is `uint64_t`, passed as `NSUInteger`
    /// (`send_id_usize_id`); both are 64-bit on every Apple target.
    pub(crate) const NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP: usize = 0x00EF_FFFF;

    // ---- the Developer Tools helper panel (`devtools_helper.rs`) ----

    /// `NSWindow.h:56,57,64,66` — `NSWindowStyleMaskTitled` (`1 << 0`)
    /// `| NSWindowStyleMaskClosable` (`1 << 1`) `| NSWindowStyleMaskUtilityWindow`
    /// (`1 << 4`) `| NSWindowStyleMaskNonactivatingPanel` (`1 << 7`)
    /// (`NSUInteger`). The helper panel's mask, set at init: a non-activating
    /// panel's mask set after init is not one AppKit honours.
    pub(crate) const NS_WINDOW_STYLE_MASK_UTILITY_PANEL: usize = 0x01 | 0x02 | 0x10 | 0x80;

    /// `NSGraphics.h:99` — `NSBackingStoreBuffered = 2` (`NSUInteger`).
    pub(crate) const NS_BACKING_STORE_BUFFERED: usize = 2;

    /// `NSWindow.h:194` — `static const NSWindowLevel NSFloatingWindowLevel =
    /// kCGFloatingWindowLevel;`, and `CGWindowLevel.h:68` —
    /// `#define kCGFloatingWindowLevel ((CGWindowLevel)3)` (`NSInteger`).
    pub(crate) const NS_FLOATING_WINDOW_LEVEL: isize = 3;

    /// `NSWindow.h:130,141` — `NSWindowCollectionBehaviorCanJoinAllSpaces`
    /// (`1 << 0`) `| NSWindowCollectionBehaviorFullScreenAuxiliary` (`1 << 8`)
    /// (`NSUInteger`): the panel follows System Settings to its Space, full
    /// screen included.
    pub(crate) const NS_WINDOW_COLLECTION_BEHAVIOR_ALL_SPACES_AUXILIARY: usize = 0x01 | 0x100;

    /// `NSDragging.h:26` — `NSDragOperationNone = 0` (`NSUInteger`): the drop
    /// was not taken.
    pub(crate) const NS_DRAG_OPERATION_NONE: usize = 0;

    /// `NSDragging.h:27` — `NSDragOperationCopy = 1` (`NSUInteger`). The only operation the
    /// helper's tile offers, in every context: a `Generic` or `Move` lets Finder move the
    /// running `/Applications/aterm.app` out of its install path.
    pub(crate) const NS_DRAG_OPERATION_COPY: usize = 1;

    /// `NSButtonCell.h:34` — `NSBezelStylePush = 1` (`NSUInteger`).
    pub(crate) const NS_BEZEL_STYLE_PUSH: usize = 1;

    /// `NSRunningApplication.h:23,27` — `NSApplicationActivateAllWindows`
    /// (`1 << 0`) `| NSApplicationActivateIgnoringOtherApps` (`1 << 1`)
    /// (`NSUInteger`). Both, for the reason `platform.rs`'s `activate_instance`
    /// gives: the second is inert on macOS 14 and later and required before it
    /// whenever another app is frontmost.
    pub(crate) const NS_APPLICATION_ACTIVATE_ALL_WINDOWS_IGNORING_OTHER_APPS: usize = 0x01 | 0x02;

    /// `NSRunningApplication.h:42` — `NSApplicationActivationPolicyProhibited`,
    /// the third enumerator of an implicit enum (`NSInteger`), so 2: a process
    /// that may not show a window or be activated. System Settings runs helper
    /// processes under its bundle id with this policy; the panel looks for the
    /// one that is not one of them.
    pub(crate) const NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED: isize = 2;

    /// `CGWindow.h:145,160` — `kCGWindowListOptionOnScreenOnly` (`1 << 0`)
    /// `| kCGWindowListExcludeDesktopElements` (`1 << 4`) (`CGWindowListOption`,
    /// `uint32_t`): the on-screen windows the panel looks for System Settings'
    /// frame among.
    pub(crate) const CG_WINDOW_LIST_ON_SCREEN_NON_DESKTOP: u32 = 0x01 | 0x10;

    /// `CGWindow.h:42` — `#define kCGNullWindowID ((CGWindowID)0)`.
    pub(crate) const CG_NULL_WINDOW_ID: u32 = 0;
}

// AppKit's exported symbols, unlike the header values in `consts`. `NSApp` and the two
// `NSAppearanceName` globals (`NSAppearance.h:63-64` declares them `APPKIT_EXTERN
// NSAppearanceName const`; `AppKit.tbd` exports both) are variables, bound as extern
// statics: each read is one load of a live framework global. `NSBeep` is a function.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    /// `NSApplication.h:196` — nil until AppKit has created the application.
    #[link_name = "NSApp"]
    static mut NS_APP: Id;

    /// `NSAppearance.h:63` — the light system appearance's name.
    #[link_name = "NSAppearanceNameAqua"]
    static NS_APPEARANCE_NAME_AQUA: Id;
    /// `NSAppearance.h:64` — the dark system appearance's name.
    #[link_name = "NSAppearanceNameDarkAqua"]
    static NS_APPEARANCE_NAME_DARK_AQUA: Id;

    /// `NSGraphics.h:222` — `APPKIT_EXTERN void NSBeep(void);`, a C function AppKit exports
    /// (`_NSBeep`), bound as a symbol like the globals above.
    #[link_name = "NSBeep"]
    fn ns_beep();
}

/// The already-created application, borrowed, or `None` before its creation.
/// Reading AppKit's global never initializes an application or contacts WindowServer.
#[must_use]
pub(crate) fn existing_application(_main_thread: MainThread) -> Option<Id> {
    // SAFETY: NSApp is AppKit's exported, nullable application pointer. AppKit
    // initializes it on the main thread; the witness keeps this load on that thread.
    let app = unsafe { NS_APP };
    (!app.is_null()).then_some(app)
}

/// Play the user's configured macOS alert sound (the BEL bell), honouring their sound
/// settings. `NSBeep()` is a free C function with no prototype a caller could get wrong, so
/// this wrapper is safe; aterm calls it from the event loop (`on_bell`).
pub(crate) fn beep() {
    // SAFETY: `_NSBeep` is exported by AppKit, takes no arguments and returns nothing, so
    // the declaration above is its complete prototype.
    unsafe { ns_beep() }
}

/// The `NSAppearanceName` for the light or dark system appearance, +0: AppKit's own global,
/// never released or retained.
#[must_use]
pub(crate) fn appearance_name(dark: bool) -> Id {
    // SAFETY: both symbols are AppKit globals that live for the whole process; reading one
    // is a plain load of an initialised `NSString *`.
    unsafe {
        if dark {
            NS_APPEARANCE_NAME_DARK_AQUA
        } else {
            NS_APPEARANCE_NAME_AQUA
        }
    }
}
