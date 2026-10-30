// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! IOKit and CoreFoundation declarations for hearing the system's sleep and wake
//! (`IOKit/pwr_mgt/IOPMLib.h`, `IOKit/IOKitLib.h`, `IOKit/IOMessage.h`,
//! `CoreFoundation/CFRunLoop.h`), held to the SDK by `tests/power_sdk.rs` (the messages, which
//! the shared declaration probe does not include, by its own probe of `IOKit/IOMessage.h`).

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use std::ffi::c_void;

/// Zero-sized `#[repr(C)]` types that only sit behind a pointer the system owns: never
/// constructed, `!Send`, `!Sync`, `!Unpin`.
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
opaque! { IONotificationPort, __CFRunLoop, __CFRunLoopSource, __CFString }

pub type kern_return_t = i32;
pub type IOReturn = kern_return_t;
pub type mach_port_t = u32;
pub type io_object_t = mach_port_t;
pub type io_connect_t = io_object_t;
pub type io_service_t = io_object_t;
pub type IONotificationPortRef = *mut IONotificationPort;
/// Called on the thread whose run loop holds the notification port's source.
pub type IOServiceInterestCallback =
    Option<unsafe extern "C" fn(*mut c_void, io_service_t, u32, *mut c_void)>;

pub type Boolean = u8;
pub type CFTimeInterval = f64;
pub type CFStringRef = *const __CFString;
pub type CFRunLoopMode = CFStringRef;
pub type CFRunLoopRef = *mut __CFRunLoop;
pub type CFRunLoopSourceRef = *mut __CFRunLoopSource;

/// `iokit_common_msg(0x270)`: the system ponders an idle sleep; answered by
/// `IOAllowPowerChange` (or `IOCancelPowerChange`, which vetoes it).
pub(crate) const kIOMessageCanSystemSleep: u32 = 0xE000_0270;
/// `iokit_common_msg(0x280)`: the system will sleep once every client allowed it, or 30 s on.
pub(crate) const kIOMessageSystemWillSleep: u32 = 0xE000_0280;
/// `iokit_common_msg(0x290)`: an idle sleep a client vetoed does not come.
pub(crate) const kIOMessageSystemWillNotSleep: u32 = 0xE000_0290;
/// `iokit_common_msg(0x300)`: the system woke, its drivers and hardware back on.
pub(crate) const kIOMessageSystemHasPoweredOn: u32 = 0xE000_0300;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    /// A session with the root power domain (`IO_OBJECT_NULL` when refused), a notification
    /// port and a notifier, each the caller's: deregister the notifier, close the session,
    /// then destroy the port.
    pub fn IORegisterForSystemPower(
        refcon: *mut c_void,
        thePortRef: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut io_object_t,
    ) -> io_connect_t;
    pub fn IODeregisterForSystemPower(notifier: *mut io_object_t) -> IOReturn;
    /// Acknowledges a can-sleep or a will-sleep by the notification id its message carried.
    pub fn IOAllowPowerChange(kernelPort: io_connect_t, notificationID: isize) -> IOReturn;
    pub fn IOServiceClose(connect: io_connect_t) -> kern_return_t;
    /// Borrowed (the Get rule): the port owns it.
    pub fn IONotificationPortGetRunLoopSource(notify: IONotificationPortRef) -> CFRunLoopSourceRef;
    pub fn IONotificationPortDestroy(notify: IONotificationPortRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopDefaultMode: CFRunLoopMode;
    /// The calling thread's run loop, made on first use; borrowed (the Get rule).
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    pub fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    /// Runs the calling thread's run loop in `mode` for at most `seconds`; returns a
    /// `CFRunLoopRunResult`.
    pub fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: CFTimeInterval,
        returnAfterSourceHandled: Boolean,
    ) -> i32;
}
