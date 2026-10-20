// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The IOKit and CoreFoundation calls the macOS arm makes to read the
//! IORegistry: the GPU's `PerformanceStatistics` (`IOAccelerator`) and the
//! device tree's CPU nodes (`IODeviceTree:/cpus`). Registry reads only: no
//! user client is opened, nothing reaches WindowServer.
//!
//! DECLARATIONS ONLY, under Apple's names, in the grammar of the shared SDK
//! probe (`crates/aterm-objc/tests/support/sdk_decls.rs`): `tests/iokit_sdk.rs`
//! reads this file as source and has clang hold every alias, constant and
//! prototype to `IOKit/IOKitLib.h` and the CoreFoundation headers, for `arm64`
//! and `x86_64`. The readers that call them live in `macos.rs`.
//!
//! Tiny-FFI posture, like `aterm-gui`'s `hid_idle`: both are system
//! frameworks every aterm process already loads, so no crate joins the
//! dependency surface. Each opaque `…Ref` points at its own zero-sized type,
//! so a `CFDataRef` does not pass where a `CFStringRef` is wanted.

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use std::ffi::{c_char, c_void};

/// Zero-sized `#[repr(C)]` types that only sit behind a pointer
/// CoreFoundation owns: never constructed, `!Send`, `!Sync`, `!Unpin`.
macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        pub struct $name {
            _opaque: [u8; 0],
            _not_send_sync_unpin:
                ::std::marker::PhantomData<(*mut u8, ::std::marker::PhantomPinned)>,
        }
    )*};
}
opaque! { __CFAllocator, __CFData, __CFDictionary, __CFNumber, __CFString }

pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;
pub type CFStringEncoding = u32;
pub type CFNumberType = CFIndex;

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const __CFAllocator;
pub type CFDataRef = *const __CFData;
pub type CFDictionaryRef = *const __CFDictionary;
pub type CFMutableDictionaryRef = *mut __CFDictionary;
pub type CFNumberRef = *const __CFNumber;
pub type CFStringRef = *const __CFString;

pub type kern_return_t = i32;
pub type mach_port_t = u32;
pub type io_object_t = mach_port_t;
pub type io_iterator_t = io_object_t;
pub type io_registry_entry_t = io_object_t;
pub type io_service_t = io_object_t;
pub type IOOptionBits = u32;

/// `CFString.h`.
pub const kCFStringEncodingUTF8: CFStringEncoding = 0x0800_0100;
/// `CFNumber.h`: every number is read out as a 64-bit signed integer.
pub const kCFNumberSInt64Type: CFNumberType = 4;

/// `IOKitLib.h`: `MACH_PORT_NULL` names the default main port
/// (`kIOMainPortDefault`, which is itself a variable that macOS 11, the arm64
/// floor, does not export).
pub(crate) const MAIN_PORT_DEFAULT: mach_port_t = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    /// A +1 `CFMutableDictionaryRef`, consumed by `IOServiceGetMatchingService`.
    pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    /// Consumes `matching`; the service it answers is the caller's (+1).
    pub fn IOServiceGetMatchingService(
        mainPort: mach_port_t,
        matching: CFDictionaryRef,
    ) -> io_service_t;
    pub fn IORegistryEntryFromPath(
        mainPort: mach_port_t,
        path: *const c_char,
    ) -> io_registry_entry_t;
    pub fn IORegistryEntryGetChildIterator(
        entry: io_registry_entry_t,
        plane: *const c_char,
        iterator: *mut io_iterator_t,
    ) -> kern_return_t;
    pub fn IOIteratorNext(iterator: io_iterator_t) -> io_object_t;
    pub fn IOObjectGetClass(object: io_object_t, className: *mut c_char) -> kern_return_t;
    pub fn IOObjectRelease(object: io_object_t) -> kern_return_t;
    /// A +1 CF object (the Create rule), or NULL.
    pub fn IORegistryEntryCreateCFProperty(
        entry: io_registry_entry_t,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: IOOptionBits,
    ) -> CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFDataGetTypeID() -> CFTypeID;
    pub fn CFDictionaryGetTypeID() -> CFTypeID;
    pub fn CFNumberGetTypeID() -> CFTypeID;
    pub fn CFStringGetTypeID() -> CFTypeID;
    pub fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        cStr: *const c_char,
        encoding: CFStringEncoding,
    ) -> CFStringRef;
    pub fn CFStringGetCString(
        theString: CFStringRef,
        buffer: *mut c_char,
        bufferSize: CFIndex,
        encoding: CFStringEncoding,
    ) -> Boolean;
    pub fn CFDictionaryGetValue(theDict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFNumberGetValue(
        number: CFNumberRef,
        theType: CFNumberType,
        valuePtr: *mut c_void,
    ) -> Boolean;
    pub fn CFDataGetLength(theData: CFDataRef) -> CFIndex;
    pub fn CFDataGetBytePtr(theData: CFDataRef) -> *const u8;
}
