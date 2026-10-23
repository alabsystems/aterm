// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's own AppKit/Foundation layer over [`aterm_objc`].

use aterm_objc::Id;

/// A witness that the current thread is the process main thread: [`aterm_objc::MainThread`] under this module's name.
pub(crate) use aterm_objc::MainThread;

/// The instantiation witness for a unit test, which never runs on the main thread.
#[cfg(test)]
pub(crate) fn test_witness() -> MainThread {
    // SAFETY: see this function's doc comment — no main-thread affinity, and the ivars never leave the worker that created them.
    unsafe { MainThread::new_unchecked() }
}

/// The typed [`aterm_objc::msg`] casts, one `unsafe fn` per C prototype rather than per selector, re-exported so call sites say `appkit::send_v_id(recv, sel!(setMenu:), menu)`.
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

    /// `NSBezierPath.h:20` — `NSLineCapStyleRound = 1` (`NSUInteger`).
    pub(crate) const NS_LINE_CAP_STYLE_ROUND: usize = 1;

    /// `NSText.h:48` — `NSTextAlignmentLeft = 0` (`NSInteger`).
    pub(crate) const NS_TEXT_ALIGNMENT_LEFT: isize = 0;

    /// `NSText.h:50/54` — `NSTextAlignmentCenter`: 1 on arm64 macOS, 2 on the x86_64 slice.
    #[cfg(target_arch = "x86_64")]
    pub(crate) const NS_TEXT_ALIGNMENT_CENTER: isize = 2;
    /// The arm64 twin: `TARGET_ABI_USES_IOS_VALUES` is true here.
    #[cfg(not(target_arch = "x86_64"))]
    pub(crate) const NS_TEXT_ALIGNMENT_CENTER: isize = 1;

    /// `NSCell.h:49` — `NSNoImage = 0` (`NSCellImagePosition`, `NSUInteger`).
    pub(crate) const NS_NO_IMAGE: usize = 0;

    /// `NSTrackingArea.h:19,29,35`.
    pub(crate) const NS_TRACKING_HOVER_IN_VISIBLE_RECT: usize = 0x01 | 0x80 | 0x200;

    /// `NSView.h:35,40` — `NSViewMinXMargin` (1) `| NSViewMaxYMargin` (32): right-anchored, top-anchored.
    pub(crate) const NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN: usize = 1 | 32;

    /// `NSView.h:36` — `NSViewWidthSizable = 2`.
    pub(crate) const NS_VIEW_WIDTH_SIZABLE: usize = 2;

    /// `NSWindow.h:215-217`.
    pub(crate) const NS_WINDOW_CLOSE_BUTTON: usize = 0;
    pub(crate) const NS_WINDOW_MINIATURIZE_BUTTON: usize = 1;
    pub(crate) const NS_WINDOW_ZOOM_BUTTON: usize = 2;

    /// `NSGraphics.h:104` — `NSWindowAbove = 1` (`NSInteger`).
    pub(crate) const NS_WINDOW_ABOVE: isize = 1;

    /// `NSToolbar.h:26` — `NSToolbarDisplayModeIconOnly = 2` (`NSUInteger`).
    pub(crate) const NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY: usize = 2;

    /// `NSWindow.h:248` — `NSWindowToolbarStyleUnifiedCompact = 4` (`NSInteger`): the single compact chrome row the strip lives in.
    pub(crate) const NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT: isize = 4;

    /// `NSWindow.h:231` — `NSWindowTitleHidden = 1` (`NSInteger`).
    pub(crate) const NS_WINDOW_TITLE_HIDDEN: isize = 1;

    /// `NSParagraphStyle.h:26,30`.
    pub(crate) const NS_LINE_BREAK_BY_TRUNCATING_TAIL: usize = 4;

    /// `NSEvent.h:103` — `NSEventMaskKeyDown = 1ULL << NSEventTypeKeyDown`, with `NSEventTypeKeyDown = 10` (`NSEvent.h:34`).
    pub(crate) const NS_EVENT_MASK_KEY_DOWN: usize = 1 << 10;

    /// `NSAlert.h:50` — `static const NSModalResponse NSAlertFirstButtonReturn = 1000;`.
    pub(crate) const NS_ALERT_FIRST_BUTTON_RETURN: isize = 1000;
    /// `NSAlertSecondButtonReturn`.
    pub(crate) const NS_ALERT_SECOND_BUTTON_RETURN: isize = 1001;

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

    /// `NSBitmapImageRep.h:37` — `NSBitmapImageFileTypePNG` (`NSUInteger`), the fifth enumerator of an implicit enum, so 4.
    pub(crate) const NS_BITMAP_IMAGE_FILE_TYPE_PNG: usize = 4;

    /// `NSGraphics.h:129` — `NSColorRenderingIntentPerceptual` (`NSInteger`), the fourth enumerator of an implicit enum, so 3.
    pub(crate) const NS_COLOR_RENDERING_INTENT_PERCEPTUAL: isize = 3;

    /// `NSProcessInfo.h:384`.
    pub(crate) const NS_ACTIVITY_USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP: usize = 0x00EF_FFFF;

    /// `NSWindow.h:56,57,64,66`.
    pub(crate) const NS_WINDOW_STYLE_MASK_UTILITY_PANEL: usize = 0x01 | 0x02 | 0x10 | 0x80;

    /// `NSGraphics.h:99` — `NSBackingStoreBuffered = 2` (`NSUInteger`).
    pub(crate) const NS_BACKING_STORE_BUFFERED: usize = 2;

    /// `NSWindow.h:194` — `static const NSWindowLevel NSFloatingWindowLevel =
    /// kCGFloatingWindowLevel;`, and `CGWindowLevel.h:68` —
    /// `#define kCGFloatingWindowLevel ((CGWindowLevel)3)` (`NSInteger`).
    pub(crate) const NS_FLOATING_WINDOW_LEVEL: isize = 3;

    /// `NSWindow.h:130,141`.
    pub(crate) const NS_WINDOW_COLLECTION_BEHAVIOR_ALL_SPACES_AUXILIARY: usize = 0x01 | 0x100;

    /// `NSDragging.h:26` — `NSDragOperationNone = 0` (`NSUInteger`): the drop was not taken.
    pub(crate) const NS_DRAG_OPERATION_NONE: usize = 0;

    /// `NSDragging.h:27` — `NSDragOperationCopy = 1` (`NSUInteger`).
    pub(crate) const NS_DRAG_OPERATION_COPY: usize = 1;

    /// `NSButtonCell.h:34` — `NSBezelStylePush = 1` (`NSUInteger`).
    pub(crate) const NS_BEZEL_STYLE_PUSH: usize = 1;

    /// `NSRunningApplication.h:23,27`.
    pub(crate) const NS_APPLICATION_ACTIVATE_ALL_WINDOWS_IGNORING_OTHER_APPS: usize = 0x01 | 0x02;

    /// `NSRunningApplication.h:42`.
    pub(crate) const NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED: isize = 2;

    /// `CGWindow.h:145,160`.
    pub(crate) const CG_WINDOW_LIST_ON_SCREEN_NON_DESKTOP: u32 = 0x01 | 0x10;

    /// `CGWindow.h:42` — `#define kCGNullWindowID ((CGWindowID)0)`.
    pub(crate) const CG_NULL_WINDOW_ID: u32 = 0;
}

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
#[must_use]
pub(crate) fn existing_application(_main_thread: MainThread) -> Option<Id> {
    // SAFETY: NSApp is AppKit's exported, nullable application pointer.
    let app = unsafe { NS_APP };
    (!app.is_null()).then_some(app)
}

/// Play the user's configured macOS alert sound (the BEL bell), honouring their sound settings.
pub(crate) fn beep() {
    // SAFETY: `_NSBeep` is exported by AppKit, takes no arguments and returns nothing, so the declaration above is its complete prototype.
    unsafe { ns_beep() }
}

/// The `NSAppearanceName` for the light or dark system appearance, +0: AppKit's own global, never released or retained.
#[must_use]
pub(crate) fn appearance_name(dark: bool) -> Id {
    // SAFETY: both symbols are AppKit globals that live for the whole process.
    unsafe {
        if dark {
            NS_APPEARANCE_NAME_DARK_AQUA
        } else {
            NS_APPEARANCE_NAME_AQUA
        }
    }
}
