// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IOKit and CoreFoundation declarations for reading macOS GPU, Neural Engine, CPU and block-storage IORegistry entries, and the GPU's user clients; and for the machine's situation (`crate::situation`): the power sources (`IOKit/ps/IOPowerSources.h`), the notify tokens that say when they changed and the states `crate::notify` reads (`notify.h`), and the run loop on which the root domain's interest and the display matching notifications arrive.

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use std::ffi::{c_char, c_int, c_void};

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
opaque! {
    __CFAllocator, __CFArray, __CFBoolean, __CFData, __CFDictionary, __CFNumber, __CFString,
    __CFRunLoop, __CFRunLoopSource, IONotificationPort,
}

pub type CFIndex = isize;
pub type CFTypeID = usize;
pub type Boolean = u8;
pub type CFStringEncoding = u32;
pub type CFNumberType = CFIndex;

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const __CFAllocator;
pub type CFArrayRef = *const __CFArray;
pub type CFBooleanRef = *const __CFBoolean;
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
pub type IONotificationPortRef = *mut IONotificationPort;
/// Called on the thread whose run loop holds the notification port's source, with the
/// message's type and argument.
pub type IOServiceInterestCallback =
    Option<unsafe extern "C" fn(*mut c_void, io_service_t, u32, *mut c_void)>;
/// Called on the same thread with the iterator a matching notification armed: draining it
/// arms it again. Apple silicon's display pipes alone are watched this way.
#[cfg(target_arch = "aarch64")]
pub type IOServiceMatchingCallback = Option<unsafe extern "C" fn(*mut c_void, io_iterator_t)>;

pub type CFHashCode = usize;
pub type CFTimeInterval = f64;
pub type CFRunLoopRef = *mut __CFRunLoop;
pub type CFRunLoopSourceRef = *mut __CFRunLoopSource;
pub type CFRunLoopMode = CFStringRef;

/// A version-0 run-loop source's context; CoreFoundation copies it at creation.
#[repr(C)]
pub struct CFRunLoopSourceContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copyDescription: Option<extern "C" fn(*const c_void) -> CFStringRef>,
    pub equal: Option<extern "C" fn(*const c_void, *const c_void) -> Boolean>,
    pub hash: Option<extern "C" fn(*const c_void) -> CFHashCode>,
    pub schedule: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFRunLoopMode)>,
    pub cancel: Option<extern "C" fn(*mut c_void, CFRunLoopRef, CFRunLoopMode)>,
    pub perform: extern "C" fn(*mut c_void),
}

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
    /// Public IOPMLib.h: every process's power assertions, keyed by its pid (a CFNumber), each
    /// an array of dictionaries; a +1 dictionary on success, released by its caller.
    pub fn IOPMCopyAssertionsByProcess(by_pid: *mut CFDictionaryRef) -> kern_return_t;
    /// A +1 `CFMutableDictionaryRef`, consumed by `IOServiceGetMatchingService`.
    pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    /// Consumes `matching`; the service it answers is the caller's (+1).
    pub fn IOServiceGetMatchingService(
        mainPort: mach_port_t,
        matching: CFDictionaryRef,
    ) -> io_service_t;
    /// Consumes `matching`; on success `existing` is a +1 iterator over every match.
    pub fn IOServiceGetMatchingServices(
        mainPort: mach_port_t,
        matching: CFDictionaryRef,
        existing: *mut io_iterator_t,
    ) -> kern_return_t;
    /// A +1 matching dictionary for the BSD device `bsdName` (`disk3s5`), consumed by
    /// `IOServiceGetMatchingServices`.
    pub fn IOBSDNameMatching(
        mainPort: mach_port_t,
        options: u32,
        bsdName: *const c_char,
    ) -> CFMutableDictionaryRef;
    /// On success `iterator` is a +1 iterator over `entry`'s parents in `plane`.
    pub fn IORegistryEntryGetParentIterator(
        entry: io_registry_entry_t,
        plane: *const c_char,
        iterator: *mut io_iterator_t,
    ) -> kern_return_t;
    /// Whether `object` is an instance of `className` or of a subclass of it.
    pub fn IOObjectConformsTo(object: io_object_t, className: *const c_char) -> boolean_t;
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
    /// A notification port of the caller's (NULL when refused), destroyed by
    /// `IONotificationPortDestroy`.
    pub fn IONotificationPortCreate(mainPort: mach_port_t) -> IONotificationPortRef;
    /// Borrowed (the Get rule): the port owns it.
    pub fn IONotificationPortGetRunLoopSource(notify: IONotificationPortRef) -> CFRunLoopSourceRef;
    pub fn IONotificationPortDestroy(notify: IONotificationPortRef);
    /// `interestType` names the interest (`IOGeneralInterest`); on success `notification` is
    /// the caller's (+1), released to end it.
    pub fn IOServiceAddInterestNotification(
        notifyPort: IONotificationPortRef,
        service: io_service_t,
        interestType: *const c_char,
        callback: IOServiceInterestCallback,
        refCon: *mut c_void,
        notification: *mut io_object_t,
    ) -> kern_return_t;
    /// Consumes `matching`; on success `notification` is a +1 iterator over the services that
    /// match now, which must be drained to arm it.
    #[cfg(target_arch = "aarch64")]
    pub fn IOServiceAddMatchingNotification(
        notifyPort: IONotificationPortRef,
        notificationType: *const c_char,
        matching: CFDictionaryRef,
        callback: IOServiceMatchingCallback,
        refCon: *mut c_void,
        notification: *mut io_iterator_t,
    ) -> kern_return_t;
    /// Public IOPowerSources.h: the power sources now, +1 (released by the caller), or NULL.
    pub fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    /// The sources in `blob`, +1, or NULL.
    pub fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
    /// Borrowed (the Get rule) while `blob` lives.
    pub fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFDictionaryRef;
    /// What provides power now (`AC Power`, `Battery Power`, `UPS Power`), borrowed while
    /// `snapshot` lives.
    pub fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
    /// The wall adapter's details (`Watts`), +1, or NULL where none is attached.
    pub fn IOPSCopyExternalPowerAdapterDetails() -> CFDictionaryRef;
}

// notify(3), in libSystem: a check token reads whether a name was posted since the last check,
// and wakes nothing.
unsafe extern "C" {
    /// Registers a check token for `name`; 0 on success.
    pub fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
    /// Sets `check` non-zero when the name was posted since the last check (and on the first
    /// check after registration); 0 on success.
    pub fn notify_check(token: c_int, check: *mut c_int) -> u32;
    /// The state last set for the token's name (`crate::notify`'s thermal pressure level); 0 on
    /// success.
    pub fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    pub fn notify_cancel(token: c_int) -> u32;
    /// Posts `name` to every process registered for it; 0 on success.
    pub fn notify_post(name: *const c_char) -> u32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    pub fn CFArrayGetTypeID() -> CFTypeID;
    pub fn CFArrayGetCount(theArray: CFArrayRef) -> CFIndex;
    /// Borrowed (the Get rule) while the array lives.
    pub fn CFArrayGetValueAtIndex(theArray: CFArrayRef, idx: CFIndex) -> *const c_void;
    pub fn CFBooleanGetTypeID() -> CFTypeID;
    pub fn CFBooleanGetValue(boolean: CFBooleanRef) -> Boolean;
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
    pub fn CFDictionaryGetCount(theDict: CFDictionaryRef) -> CFIndex;
    /// Fills `keys` and `values`, each room for `CFDictionaryGetCount` pointers, with borrowed
    /// pointers (the Get rule) valid while the dictionary lives.
    pub fn CFDictionaryGetKeysAndValues(
        theDict: CFDictionaryRef,
        keys: *mut *const c_void,
        values: *mut *const c_void,
    );
    pub fn CFNumberGetValue(
        number: CFNumberRef,
        theType: CFNumberType,
        valuePtr: *mut c_void,
    ) -> Boolean;
    pub fn CFDataGetLength(theData: CFDataRef) -> CFIndex;
    pub fn CFDataGetBytePtr(theData: CFDataRef) -> *const u8;
    pub static kCFRunLoopDefaultMode: CFRunLoopMode;
    /// The calling thread's run loop, made on first use; borrowed (the Get rule).
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    /// Thread-safe: a source signalled before its run loop runs is handled at the next run.
    pub fn CFRunLoopSourceCreate(
        allocator: CFAllocatorRef,
        order: CFIndex,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
    pub fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);
    /// Wakes `rl` from its wait, from any thread.
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    /// Ends the current run of `rl`; a stop while it is not running is lost.
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    /// Runs the calling thread's run loop in `mode` for at most `seconds`; returns a
    /// `CFRunLoopRunResult`.
    pub fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: CFTimeInterval,
        returnAfterSourceHandled: Boolean,
    ) -> i32;
}

// The calls a test makes to read a recorded registry entry (`ioreg -a`'s XML) back as the
// CoreFoundation objects a live read gives.
#[cfg(test)]
opaque! { __CFError }

/// `CFBase.h`: `unsigned long`.
#[cfg(test)]
pub type CFOptionFlags = usize;
/// `CFPropertyList.h`: a `CFIndex` enum.
#[cfg(test)]
pub type CFPropertyListFormat = CFIndex;
#[cfg(test)]
pub type CFErrorRef = *mut __CFError;

#[cfg(test)]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    /// A +1 `CFDataRef` holding a copy of `length` bytes, or NULL.
    pub fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    /// A +1 property list read from `data`, or NULL (`error`, where not NULL, is then +1).
    pub fn CFPropertyListCreateWithData(
        allocator: CFAllocatorRef,
        data: CFDataRef,
        options: CFOptionFlags,
        format: *mut CFPropertyListFormat,
        error: *mut CFErrorRef,
    ) -> CFTypeRef;
}

// The calls that hold a power assertion of this process's own (`crate::power::hold`), which
// `IOPMCopyAssertionsByProcess` then lists under its pid.
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    /// Public IOPMLib.h: `level` 255 is `kIOPMAssertionLevelOn`; `id` is written on success.
    pub fn IOPMAssertionCreateWithName(
        kind: CFStringRef,
        level: u32,
        name: CFStringRef,
        id: *mut u32,
    ) -> kern_return_t;
    pub fn IOPMAssertionRelease(id: u32) -> kern_return_t;
}
