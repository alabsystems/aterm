// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`super::System::automation_consent`] on the live system: macOS's Automation answer for the
//! app it holds responsible for this process sending Apple events to System Events, read with
//! `AEDeterminePermissionToAutomateTarget` and `askUserIfNeeded` off, so macOS shows nothing and
//! the call does not wait for a person. CoreServices is opened at the first look (`dlopen`); the
//! call reaches `tccd` through `appleeventsd`, never WindowServer.

use super::AutomationConsent;
use std::ffi::{CStr, c_char, c_int, c_void};

/// The Apple Event Manager, through its umbrella framework.
const CORE_SERVICES: &CStr = c"/System/Library/Frameworks/CoreServices.framework/CoreServices";

/// `<dlfcn.h>`'s `RTLD_LAZY` on Darwin.
const RTLD_LAZY: c_int = 0x1;

/// System Events, the target the sign-outs' clicks go through.
const SYSTEM_EVENTS: &[u8] = b"com.apple.systemevents";

/// `typeApplicationBundleID` (`'bund'`).
const TYPE_APPLICATION_BUNDLE_ID: u32 = u32::from_be_bytes(*b"bund");

/// `typeWildCard` (`'****'`): any event class and id.
const TYPE_WILD_CARD: u32 = u32::from_be_bytes(*b"****");

/// `errAEEventNotPermitted`: refused.
const NOT_PERMITTED: i32 = -1743;

/// `errAEEventWouldRequireUserConsent`: macOS would have to ask.
const WOULD_REQUIRE_CONSENT: i32 = -1744;

/// `<AEDataModel.h>`'s `AEDesc`, under that header's two-byte packing.
#[repr(C, packed(2))]
struct AeDesc {
    descriptor_type: u32,
    data_handle: *mut c_void,
}

impl AeDesc {
    /// `typeNull`'s empty descriptor, as `AEInitializeDesc` leaves one.
    const fn null() -> Self {
        Self {
            descriptor_type: u32::from_be_bytes(*b"null"),
            data_handle: std::ptr::null_mut(),
        }
    }
}

type Create = unsafe extern "C" fn(u32, *const c_void, isize, *mut AeDesc) -> i16;
type Dispose = unsafe extern "C" fn(*mut AeDesc) -> i16;
type Determine = unsafe extern "C" fn(*const AeDesc, u32, u32, u8) -> i32;

// SAFETY: libSystem's `dlopen`, with `<dlfcn.h>`'s prototype; aterm's `libc` declares
// `dlsym` but not it.
unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
}

/// `path`'s handle, kept for the process's life (never closed), or `None`.
fn open(path: &CStr) -> Option<*mut c_void> {
    // SAFETY: a NUL-terminated path of a system framework in the shared cache.
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY) };
    (!handle.is_null()).then_some(handle)
}

/// `name` in `handle`, or `None`.
fn symbol(handle: *mut c_void, name: &CStr) -> Option<*mut c_void> {
    // SAFETY: a live handle and a NUL-terminated name.
    let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
    (!found.is_null()).then_some(found)
}

/// A System Events address descriptor, disposed of when it drops.
struct Target {
    desc: AeDesc,
    dispose: Dispose,
}

impl Target {
    fn new(services: *mut c_void) -> Option<Self> {
        let create = symbol(services, c"AECreateDesc")?;
        let dispose = symbol(services, c"AEDisposeDesc")?;
        // SAFETY: CoreServices' `OSErr AECreateDesc(DescType, const void *, Size, AEDesc *)` and
        // `OSErr AEDisposeDesc(AEDesc *)` (`OSErr` is an `SInt16`, `Size` a `long`); a function
        // pointer and a data pointer share their width on every Mac.
        let (create, dispose) = unsafe {
            (
                std::mem::transmute::<*mut c_void, Create>(create),
                std::mem::transmute::<*mut c_void, Dispose>(dispose),
            )
        };
        let mut desc = AeDesc::null();
        let len = isize::try_from(SYSTEM_EVENTS.len()).ok()?;
        // SAFETY: the data pointer and its length describe a live static slice, which the call
        // copies; `desc` is a writable descriptor it fills.
        let created = unsafe {
            create(
                TYPE_APPLICATION_BUNDLE_ID,
                SYSTEM_EVENTS.as_ptr().cast(),
                len,
                &raw mut desc,
            )
        };
        // Built only once the call filled it: a `Target` disposes of its descriptor on drop.
        (created == 0).then(|| Self { desc, dispose })
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        // SAFETY: a descriptor `AECreateDesc` filled, disposed of exactly once.
        let _ = unsafe { (self.dispose)(&raw mut self.desc) };
    }
}

/// What `AEDeterminePermissionToAutomateTarget` answered, as [`AutomationConsent`].
const fn consent_of(status: i32) -> AutomationConsent {
    match status {
        0 => AutomationConsent::Allowed,
        NOT_PERMITTED => AutomationConsent::Refused,
        WOULD_REQUIRE_CONSENT => AutomationConsent::Unanswered,
        // `procNotFound` (-600): System Events is not running, so macOS cannot say; any other
        // status says nothing either.
        _ => AutomationConsent::Unread,
    }
}

/// `AEDeterminePermissionToAutomateTarget(System Events, '****', '****', false)`.
pub(super) fn consent() -> AutomationConsent {
    let Some(services) = open(CORE_SERVICES) else {
        return AutomationConsent::Unread;
    };
    let (Some(determine), Some(target)) = (
        symbol(services, c"AEDeterminePermissionToAutomateTarget"),
        Target::new(services),
    ) else {
        return AutomationConsent::Unread;
    };
    // SAFETY: CoreServices' `OSStatus AEDeterminePermissionToAutomateTarget(const AEAddressDesc
    // *, AEEventClass, AEEventID, Boolean)` (`OSStatus` is an `SInt32`, `Boolean` an unsigned
    // char); the function and data pointers share their width.
    let determine = unsafe { std::mem::transmute::<*mut c_void, Determine>(determine) };
    // SAFETY: a live descriptor, read only; `askUserIfNeeded` false, so macOS asks nothing.
    let status = unsafe { determine(&raw const target.desc, TYPE_WILD_CARD, TYPE_WILD_CARD, 0) };
    consent_of(status)
}

#[cfg(test)]
#[path = "automation_tests.rs"]
mod tests;
