// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Keyboard-layout and secure-input Carbon exports, loaded at the first native use.

use crate::framework::Framework;
use aterm_objc::cf::CFStringRef;
use std::ffi::c_void;

static CARBON: Framework = Framework::new(&[c"/System/Library/Frameworks/Carbon.framework/Carbon"]);

#[repr(transparent)]
pub struct TISInputSource(c_void);
pub type TISInputSourceRef = *mut TISInputSource;
#[repr(transparent)]
pub struct UCKeyboardLayout(c_void);
pub type OptionBits = u32;
pub type UniCharCount = std::os::raw::c_ulong;
pub type UniChar = std::os::raw::c_ushort;
pub type OSStatus = i32;

framework_constants! { CARBON;
    /// Unicode keyboard-layout input-source property key, borrowed for the process lifetime.
    fn unicode_key_layout_data() -> CFStringRef = c"kTISPropertyUnicodeKeyLayoutData";
}

framework_functions! { CARBON, "C";
    /// Read a property borrowed from an input source.
    /// # Safety
    /// `source` and `key` must remain live and have the TIS input-source and CFString types.
    fn input_source_property(source: TISInputSourceRef, key: CFStringRef) -> *mut c_void = c"TISGetInputSourceProperty";
    /// Copy the current keyboard-layout source; the caller releases the returned reference.
    /// # Safety
    /// Call on the main thread, as the Text Input Sources API requires.
    fn copy_current_keyboard_layout_input_source() -> TISInputSourceRef = c"TISCopyCurrentKeyboardLayoutInputSource";
    /// Read the keyboard type.
    /// # Safety
    /// Call on the main thread alongside the selected keyboard layout.
    fn keyboard_type() -> u8 = c"LMGetKbdType";
    /// Translate a key with a Unicode keyboard-layout resource.
    /// # Safety
    /// `layout` must be a live Unicode layout; the state and count pointers must be writable,
    /// and `output` must hold `capacity` UniChars. The resource must outlive the call.
    #[expect(clippy::too_many_arguments, reason = "UCKeyTranslate's Carbon ABI has ten parameters; the wrapper preserves that exact prototype")]
    fn key_translate(layout: *const UCKeyboardLayout, key: u16, action: u16,
        modifiers: u32, keyboard_type: u32, options: OptionBits, dead: *mut u32,
        capacity: UniCharCount, count: *mut UniCharCount, output: *mut UniChar) -> OSStatus = c"UCKeyTranslate";
    /// Acquire one process secure-event-input count.
    /// # Safety
    /// Balance successful acquisitions with releases and satisfy the native focus policy.
    fn enable_secure_event_input() -> OSStatus = c"EnableSecureEventInput";
    /// Release one process secure-event-input count.
    /// # Safety
    /// The process must hold a successful acquisition that this call balances.
    fn disable_secure_event_input() -> OSStatus = c"DisableSecureEventInput";
}
