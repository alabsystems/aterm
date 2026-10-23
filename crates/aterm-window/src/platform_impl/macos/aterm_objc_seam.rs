// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use aterm_objc::Id;

/// Allocates a retained `NSString` for `s`, or returns `None` if Foundation refused it.
#[must_use]
pub(super) fn nsstring(s: &str) -> Option<aterm_objc::Obj> {
    aterm_objc::ns_string(s)
}

/// Copies an `NSString` into a Rust `String`, returning `String::new()` for nil.
///
/// # Safety
/// `s` must be nil or a valid `NSString`.
#[must_use]
pub(super) unsafe fn nsstring_to_rust(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: The caller guarantees `s` is a valid `NSString`.
    unsafe { aterm_objc::ns_string_to_rust(s) }
}

pub(super) mod consts {
    pub(crate) const NS_WINDOW_STYLE_MASK_TITLED: usize = 1 << 0;
    pub(crate) const NS_WINDOW_STYLE_MASK_CLOSABLE: usize = 1 << 1;
    pub(crate) const NS_WINDOW_STYLE_MASK_MINIATURIZABLE: usize = 1 << 2;
    pub(crate) const NS_WINDOW_STYLE_MASK_RESIZABLE: usize = 1 << 3;

    pub(crate) const NS_WINDOW_OCCLUSION_STATE_VISIBLE: usize = 1 << 1;

    pub(crate) const NS_APPLICATION_ACTIVATION_POLICY_REGULAR: isize = 0;
    pub(crate) const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;
    pub(crate) const NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED: isize = 2;

    pub(crate) const NS_CRITICAL_REQUEST: usize = 0;
    pub(crate) const NS_INFORMATIONAL_REQUEST: usize = 10;

    pub(crate) const NS_BACKING_STORE_BUFFERED: usize = 2;

    pub(crate) const NS_DRAG_OPERATION_NONE: usize = 0;
    pub(crate) const NS_DRAG_OPERATION_COPY: usize = 1;

    pub(crate) const NS_KEY_VALUE_OBSERVING_OPTION_NEW: usize = 0x01;
    pub(crate) const NS_KEY_VALUE_OBSERVING_OPTION_OLD: usize = 0x02;

    /// `NSAppKitVersionNumber10_12` (`1504.0`).
    pub(crate) const NS_APPKIT_VERSION_NUMBER_10_12: f64 = 1504.0;

    pub(crate) const NS_EVENT_MODIFIER_FLAG_SHIFT: usize = 1 << 17;
    pub(crate) const NS_EVENT_MODIFIER_FLAG_CONTROL: usize = 1 << 18;
    pub(crate) const NS_EVENT_MODIFIER_FLAG_OPTION: usize = 1 << 19;
    pub(crate) const NS_EVENT_MODIFIER_FLAG_COMMAND: usize = 1 << 20;

    pub(crate) const NS_EVENT_TYPE_KEY_UP: usize = 11;
    pub(crate) const NS_EVENT_TYPE_APPLICATION_DEFINED: usize = 15;

    pub(crate) const NS_EVENT_SUBTYPE_WINDOW_EXPOSED: i16 = 0;

    pub(crate) const NS_EVENT_PHASE_BEGAN: usize = 1 << 0;
    pub(crate) const NS_EVENT_PHASE_ENDED: usize = 1 << 3;
    pub(crate) const NS_EVENT_PHASE_CANCELLED: usize = 1 << 4;
    pub(crate) const NS_EVENT_PHASE_MAY_BEGIN: usize = 1 << 5;

    /// `NSNotFound` (`NSIntegerMax`).
    pub(crate) const NS_NOT_FOUND: usize = 0x7fff_ffff_ffff_ffff;

    /// `NSUTF8StringEncoding` (`4`).
    pub(crate) const NS_UTF8_STRING_ENCODING: usize = 4;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    /// Running AppKit version (`NSAppKitVersionNumber`).
    #[link_name = "NSAppKitVersionNumber"]
    pub(super) static NS_APPKIT_VERSION_NUMBER: f64;
    /// Legacy filenames pasteboard type (`NSFilenamesPboardType`).
    #[link_name = "NSFilenamesPboardType"]
    pub(super) static NS_FILENAMES_PBOARD_TYPE: Id;
    /// Light system appearance name (`NSAppearanceNameAqua`).
    #[link_name = "NSAppearanceNameAqua"]
    pub(super) static NS_APPEARANCE_NAME_AQUA: Id;
    /// Notification posted when an `NSView`'s frame changes (`NSViewFrameDidChangeNotification`).
    #[link_name = "NSViewFrameDidChangeNotification"]
    pub(super) static NS_VIEW_FRAME_DID_CHANGE_NOTIFICATION: Id;
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    /// KVO change dictionary new-value key (`NSKeyValueChangeNewKey`).
    #[link_name = "NSKeyValueChangeNewKey"]
    pub(super) static NS_KEY_VALUE_CHANGE_NEW_KEY: Id;
    /// KVO change dictionary old-value key (`NSKeyValueChangeOldKey`).
    #[link_name = "NSKeyValueChangeOldKey"]
    pub(super) static NS_KEY_VALUE_CHANGE_OLD_KEY: Id;
}
