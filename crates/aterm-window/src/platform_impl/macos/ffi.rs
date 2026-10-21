// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/macos/ffi.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE.
// (The CoreFoundation/CoreGraphics types come from `aterm_objc::cf`/`cg`, not
//  the `core-foundation`/`core-graphics` crates.)
// TODO: Upstream these

// LOCAL PATCH (aterm), 2026-10-15 (winit-core step 5): `dead_code` is no longer allowed here;
// the declarations nothing called (ten `CGError` values, eleven IOKit pixel-encoding strings,
// `CGConfigureDisplayFadeEffect`) are cut, and the rest are all live.
//
// LOCAL PATCH (aterm), 2026-10-16 (winit-core step 5): and so were their users' declarations
// when the macOS carve took exclusive fullscreen (display capture and release, the fade
// reservation, the shielding level, setting and listing display modes and their three
// pixel-encoding strings), window levels and blur (the two private SkyLight calls). What is
// left reads a display's UUID and current refresh rate, and the keyboard layout.
#![allow(non_snake_case, non_upper_case_globals)]

use std::ffi::c_void;

use aterm_cg::CGDirectDisplayID;
use aterm_objc::cf::{Boolean, CFStringRef, CFUUIDRef};

pub const TRUE: Boolean = 1;

pub type CGDisplayModeRef = *mut c_void;

// `CGDisplayCreateUUIDFromDisplayID` comes from the `ColorSync` framework.
// However, that framework was only introduced "publicly" in macOS 10.13.
//
// Since we want to support older versions, we can't link to `ColorSync`
// directly. Fortunately, it has always been available as a subframework of
// `ApplicationServices`, see:
// https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/OSX_Technology_Overview/SystemFrameworks/SystemFrameworks.html#//apple_ref/doc/uid/TP40001067-CH210-BBCFFIEG
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

    // CVBase.h

    pub type CVTimeFlags = i32; // int32_t
    pub const kCVTimeIsIndefinite: CVTimeFlags = 1 << 0;

    #[repr(C)]
    #[derive(Debug, Clone)]
    pub struct CVTime {
        pub time_value: i64, // int64_t
        pub time_scale: i32, // int32_t
        pub flags: i32,      // int32_t
    }

    // CVReturn.h

    pub type CVReturn = i32; // int32_t
    pub const kCVReturnSuccess: CVReturn = 0;

    // CVDisplayLink.h

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
