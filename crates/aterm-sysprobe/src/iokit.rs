// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IOKit and CoreFoundation declarations for reading macOS GPU and CPU IORegistry entries, and the GPU's user clients.

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
opaque! { __CFAllocator, __CFArray, __CFData, __CFDictionary, __CFNumber, __CFString }

pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;
pub type CFStringEncoding = u32;
pub type CFNumberType = CFIndex;

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const __CFAllocator;
pub type CFArrayRef = *const __CFArray;
pub type CFDataRef = *const __CFData;
pub type CFDictionaryRef = *const __CFDictionary;
pub type CFMutableDictionaryRef = *mut __CFDictionary;
pub type CFNumberRef = *const __CFNumber;
pub type CFStringRef = *const __CFString;

pub type kern_return_t = i32;
/// `mach/arm/boolean.h`: `int`.
#[cfg(target_arch = "aarch64")]
pub type boolean_t = i32;
/// `mach/i386/boolean.h`: `unsigned int` in user space.
#[cfg(target_arch = "x86_64")]
pub type boolean_t = u32;
pub type mach_port_t = u32;
pub type io_object_t = mach_port_t;
pub type io_iterator_t = io_object_t;
pub type io_registry_entry_t = io_object_t;
pub type io_service_t = io_object_t;
pub type IOOptionBits = u32;

/// `mach/processor_info.h`: selectors and layout for online processor slots.
pub const PROCESSOR_BASIC_INFO: i32 = 1;
pub const PROCESSOR_BASIC_INFO_COUNT: u32 = 5;

#[repr(C)]
pub struct processor_basic_info_data_t {
    pub cpu_type: i32,
    pub cpu_subtype: i32,
    pub running: boolean_t,
    pub slot_num: i32,
    pub is_main: boolean_t,
}

/// `CFString.h`.
pub const kCFStringEncodingUTF8: CFStringEncoding = 0x0800_0100;
/// `CFNumber.h`: every number is read out as a 64-bit signed integer.
pub const kCFNumberSInt64Type: CFNumberType = 4;

/// Default main port (`MACH_PORT_NULL`, replacing `kIOMainPortDefault` on macOS 11).
pub(crate) const MAIN_PORT_DEFAULT: mach_port_t = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    /// Public IOPMLib.h; a +1 dictionary on success, released by its caller.
    pub fn IOPMCopyCPUPowerStatus(status: *mut CFDictionaryRef) -> kern_return_t;
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
    /// False once the structure the iterator walks changed under it: an `IOIteratorNext` that
    /// returned 0 then ended the walk early, not at its end.
    pub fn IOIteratorIsValid(iterator: io_iterator_t) -> boolean_t;
    pub fn IOObjectGetClass(object: io_object_t, className: *mut c_char) -> kern_return_t;
    pub fn IOObjectRelease(object: io_object_t) -> kern_return_t;
    /// The entry's registry-wide id, stable while it lives.
    pub fn IORegistryEntryGetRegistryEntryID(
        entry: io_registry_entry_t,
        entryID: *mut u64,
    ) -> kern_return_t;
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
    pub fn CFArrayGetTypeID() -> CFTypeID;
    pub fn CFArrayGetCount(theArray: CFArrayRef) -> CFIndex;
    /// Borrowed (the Get rule) while the array lives.
    pub fn CFArrayGetValueAtIndex(theArray: CFArrayRef, idx: CFIndex) -> *const c_void;
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
