// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/ffi.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm): the CoreFoundation and CoreGraphics types come from `aterm_objc::cf` and
// `aterm_cg`, not the `core-foundation`/`core-graphics` crates. `dead_code` is not allowed: only
// what is called is declared (a display's UUID and current refresh rate, and the keyboard
// layout). Upstream's `CGError` values, IOKit pixel-encoding strings, display fade, capture and
// mode calls, shielding level, and SkyLight blur and window-level calls went with exclusive
// fullscreen, window levels and blur.
#![allow(non_snake_case, non_upper_case_globals)]

use std::ffi::c_void;

use aterm_cg::CGDirectDisplayID;
use aterm_objc::cf::{Boolean, CFStringRef, CFUUIDRef};

pub const TRUE: Boolean = 1;

pub type CGDisplayModeRef = *mut c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> CFUUIDRef;

    pub fn CGDisplayGetDisplayIDFromUUID(uuid: CFUUIDRef) -> CGDirectDisplayID;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGDisplayModeGetRefreshRate(mode: CGDisplayModeRef) -> f64;
    pub fn CGDisplayModeRelease(mode: CGDisplayModeRef);
}

mod core_video {
    use super::*;

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {}

    pub type CVTimeFlags = i32; // int32_t
    pub const kCVTimeIsIndefinite: CVTimeFlags = 1 << 0;

    #[repr(C)]
    #[derive(Debug, Clone)]
    pub struct CVTime {
        pub time_value: i64, // int64_t
        pub time_scale: i32, // int32_t
        pub flags: i32,      // int32_t
    }

    pub type CVReturn = i32; // int32_t
    pub const kCVReturnSuccess: CVReturn = 0;

    pub type CVDisplayLinkRef = *mut c_void;

    extern "C" {
        pub fn CVDisplayLinkCreateWithCGDisplay(
            displayID: CGDirectDisplayID,
            displayLinkOut: *mut CVDisplayLinkRef,
        ) -> CVReturn;
        pub fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(
            displayLink: CVDisplayLinkRef,
        ) -> CVTime;
        pub fn CVDisplayLinkRelease(displayLink: CVDisplayLinkRef);
    }
}

pub use core_video::*;
#[repr(transparent)]
pub struct TISInputSource(std::ffi::c_void);
pub type TISInputSourceRef = *mut TISInputSource;

#[repr(transparent)]
pub struct UCKeyboardLayout(std::ffi::c_void);

pub type OptionBits = u32;
pub type UniCharCount = std::os::raw::c_ulong;
pub type UniChar = std::os::raw::c_ushort;
pub type OSStatus = i32;

#[allow(non_upper_case_globals)]
pub const kUCKeyActionDown: u16 = 0;
#[allow(non_upper_case_globals)]
pub const kUCKeyActionDisplay: u16 = 3;
#[allow(non_upper_case_globals)]
pub const kUCKeyTranslateNoDeadKeysMask: OptionBits = 1;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    pub static kTISPropertyUnicodeKeyLayoutData: CFStringRef;

    #[allow(non_snake_case)]
    pub fn TISGetInputSourceProperty(
        inputSource: TISInputSourceRef,
        propertyKey: CFStringRef,
    ) -> *mut c_void;

    pub fn TISCopyCurrentKeyboardLayoutInputSource() -> TISInputSourceRef;

    pub fn LMGetKbdType() -> u8;

    #[allow(non_snake_case)]
    pub fn UCKeyTranslate(
        keyLayoutPtr: *const UCKeyboardLayout,
        virtualKeyCode: u16,
        keyAction: u16,
        modifierKeyState: u32,
        keyboardType: u32,
        keyTranslateOptions: OptionBits,
        deadKeyState: *mut u32,
        maxStringLength: UniCharCount,
        actualStringLength: *mut UniCharCount,
        unicodeString: *mut UniChar,
    ) -> OSStatus;
}
