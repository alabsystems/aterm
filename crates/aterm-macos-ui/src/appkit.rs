// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Typed AppKit exports, resolved at their first use instead of process launch.

use crate::framework::{Framework, Variable};
use aterm_objc::{CGRect, Id, MainThread};
use std::sync::OnceLock;

static APPKIT: Framework = Framework::new(&[c"/System/Library/Frameworks/AppKit.framework/AppKit"]);

/// Load AppKit before looking up its Objective-C classes. This creates no application.
pub fn load() {
    APPKIT.load();
}

/// Look up a class in a native AppKit context, loading its framework first.
/// This registers the framework's classes without creating an application.
pub fn class(name: &'static std::ffi::CStr) -> aterm_objc::ClassPtr {
    APPKIT.class(name)
}

/// The existing application's borrowed pointer, or nil before its creation.
pub fn application(_main: MainThread) -> Id {
    static APPLICATION: OnceLock<Variable<Id>> = OnceLock::new();
    // SAFETY: NSApp is an exported id variable, initialized to nil.
    let variable = APPLICATION.get_or_init(|| unsafe { APPKIT.variable(c"NSApp") });
    // SAFETY: AppKit updates NSApp on the main thread; the witness serializes this read.
    unsafe { variable.read() }
}

framework_constants! { APPKIT;
    /// The light system appearance name, borrowed for the process lifetime.
    fn appearance_name_aqua() -> Id = c"NSAppearanceNameAqua";
    /// The dark system appearance name, borrowed for the process lifetime.
    fn appearance_name_dark_aqua() -> Id = c"NSAppearanceNameDarkAqua";
    /// Running AppKit version.
    fn version_number() -> f64 = c"NSAppKitVersionNumber";
    /// Legacy filenames pasteboard type.
    fn filenames_pboard_type() -> Id = c"NSFilenamesPboardType";
    /// View frame-change notification name.
    fn view_frame_did_change_notification() -> Id = c"NSViewFrameDidChangeNotification";
    /// UTF-8 plain-text pasteboard type.
    fn pasteboard_type_string() -> Id = c"NSPasteboardTypeString";
    /// Main run-loop mode used for menu and drag tracking.
    fn event_tracking_run_loop_mode() -> Id = c"NSEventTrackingRunLoopMode";
    /// Accessibility announcement notification name.
    fn accessibility_announcement_requested_notification() -> Id = c"NSAccessibilityAnnouncementRequestedNotification";
    /// Accessibility announcement text dictionary key.
    fn accessibility_announcement_key() -> Id = c"NSAccessibilityAnnouncementKey";
    /// Accessibility announcement priority dictionary key.
    fn accessibility_priority_key() -> Id = c"NSAccessibilityPriorityKey";
    /// Accessibility layout-change notification name.
    fn accessibility_layout_changed_notification() -> Id = c"NSAccessibilityLayoutChangedNotification";
    /// Accessibility value-change notification name.
    fn accessibility_value_changed_notification() -> Id = c"NSAccessibilityValueChangedNotification";
    /// Accessibility focus-change notification name.
    fn accessibility_focused_ui_element_changed_notification() -> Id = c"NSAccessibilityFocusedUIElementChangedNotification";
}

framework_functions! { APPKIT, "C";
    /// Play the configured system alert sound.
    /// # Safety
    /// Call only where AppKit permits playing a system alert.
    fn beep() = c"NSBeep";
}

framework_functions! { APPKIT, "C-unwind";
    /// Convert a view-local accessibility frame into screen points.
    /// # Safety
    /// `view` must be a live NSView, called on the main thread.
    fn accessibility_frame_in_view(view: Id, frame: CGRect) -> CGRect = c"NSAccessibilityFrameInView";
    /// Post an accessibility notification.
    /// # Safety
    /// Both objects must be live and suitable for this main-thread notification.
    fn accessibility_post_notification(element: Id, notification: Id) = c"NSAccessibilityPostNotification";
    /// Post an accessibility notification with its dictionary.
    /// # Safety
    /// Objects must remain live; `info` must be the notification's NSDictionary, on the main thread.
    fn accessibility_post_notification_with_user_info(element: Id, notification: Id, info: Id) = c"NSAccessibilityPostNotificationWithUserInfo";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_first_use_reads_real_immutable_exports() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    aterm_objc::autoreleasepool(|_| {
                        let aqua = appearance_name_aqua();
                        assert!(!aqua.is_null());
                        assert!(!appearance_name_dark_aqua().is_null());
                        assert!(!pasteboard_type_string().is_null());
                        assert!(!event_tracking_run_loop_mode().is_null());
                        assert!(!aterm_objc::class(c"NSApplication").is_null());
                        assert!(version_number().is_finite() && version_number() > 0.0);
                        // SAFETY: AppKit exports this immutable NSString for the process lifetime.
                        let text = unsafe { aterm_objc::ns_string_to_rust(aqua) };
                        assert!(!text.is_empty());
                        (aqua.as_ptr().addr(), text)
                    })
                })
            })
            .collect();
        let readings: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(readings.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
