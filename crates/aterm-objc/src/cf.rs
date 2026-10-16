// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CoreFoundation aterm calls: the run loop (sources, observers, wake-ups),
//! retain/release, and the few objects the windowing backend reads — a list of
//! display modes, a keyboard layout's bytes, a display's UUID, a pixel-encoding
//! string.
//!
//! Declarations under Apple's names, plus two owners ([`CFUUID`], [`CFString`])
//! where a +1 is held past one call. Each opaque `…Ref` points at its own
//! zero-sized type, so a `CFArrayRef` does not pass where a `CFStringRef` is
//! wanted.
//!
//! `tests/cf_cg_sdk.rs` reads this file and `cg.rs` as source and has clang
//! hold every alias, struct field, constant, static and prototype to the SDK
//! headers, for `arm64` and `x86_64`.
//!
//! It replaces the `core-foundation` crates in the vendored winit fork
//! (`docs/AUDIT-third-party-2026-10-15.md`, winit-core step 3). The run-loop
//! timer is not here: it is [`crate::wake_timer`]'s alone (the `runloop-timer`
//! guard in `aterm-census --guards`).

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

/// Zero-sized `#[repr(C)]` types that only ever sit behind a pointer
/// CoreFoundation owns: never constructed here, `!Send`, `!Sync` and `!Unpin`.
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
#[cfg(feature = "cg")]
pub(crate) use opaque;

opaque! {
    __CFAllocator, __CFArray, __CFData, __CFDictionary, __CFString, __CFUUID,
    __CFRunLoop, __CFRunLoopObserver, __CFRunLoopSource,
}

pub type CFIndex = isize;
pub type CFOptionFlags = usize;
pub type CFHashCode = usize;
pub type CFTimeInterval = f64;
pub type Boolean = u8;

pub type CFTypeRef = *const c_void;
pub type CFAllocatorRef = *const __CFAllocator;
pub type CFArrayRef = *const __CFArray;
pub type CFDataRef = *const __CFData;
pub type CFDictionaryRef = *const __CFDictionary;
pub type CFStringRef = *const __CFString;
pub type CFUUIDRef = *const __CFUUID;
pub type CFRunLoopRef = *mut __CFRunLoop;
pub type CFRunLoopObserverRef = *mut __CFRunLoopObserver;
pub type CFRunLoopSourceRef = *mut __CFRunLoopSource;
pub type CFRunLoopMode = CFStringRef;
pub type CFRunLoopActivity = CFOptionFlags;
pub type CFRunLoopRunResult = i32;

/// The observer callout. `"C-unwind"` because winit's observers are defined
/// that way (a Rust panic is caught inside them, an `NSException` contained):
/// the ABI string tells rustc only whether the callee may unwind, so this is
/// the same C function pointer CoreFoundation calls, minus a transmute at the
/// registration site.
pub type CFRunLoopObserverCallBack =
    extern "C-unwind" fn(CFRunLoopObserverRef, CFRunLoopActivity, *mut c_void);

pub const kCFRunLoopBeforeWaiting: CFRunLoopActivity = 1 << 5;
pub const kCFRunLoopAfterWaiting: CFRunLoopActivity = 1 << 6;
pub const kCFRunLoopExit: CFRunLoopActivity = 1 << 7;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CFRange {
    pub location: CFIndex,
    pub length: CFIndex,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CFUUIDBytes {
    pub byte0: u8,
    pub byte1: u8,
    pub byte2: u8,
    pub byte3: u8,
    pub byte4: u8,
    pub byte5: u8,
    pub byte6: u8,
    pub byte7: u8,
    pub byte8: u8,
    pub byte9: u8,
    pub byte10: u8,
    pub byte11: u8,
    pub byte12: u8,
    pub byte13: u8,
    pub byte14: u8,
    pub byte15: u8,
}

/// A version-0 source's context; CoreFoundation copies it at creation.
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

/// An observer's context; CoreFoundation copies it at creation.
#[repr(C)]
pub struct CFRunLoopObserverContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copyDescription: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopCommonModes: CFRunLoopMode;
    pub static kCFRunLoopDefaultMode: CFRunLoopMode;

    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);

    pub fn CFArrayGetCount(the_array: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: CFIndex) -> *const c_void;
    pub fn CFDataGetBytePtr(the_data: CFDataRef) -> *const u8;
    pub fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
    pub fn CFStringGetCharacters(the_string: CFStringRef, range: CFRange, buffer: *mut u16);
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;

    pub fn CFRunLoopGetMain() -> CFRunLoopRef;
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
    pub fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);
    pub fn CFRunLoopSourceCreate(
        allocator: CFAllocatorRef,
        order: CFIndex,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    pub fn CFRunLoopSourceSignal(source: CFRunLoopSourceRef);
    pub fn CFRunLoopSourceInvalidate(source: CFRunLoopSourceRef);
    pub fn CFRunLoopAddObserver(
        rl: CFRunLoopRef,
        observer: CFRunLoopObserverRef,
        mode: CFRunLoopMode,
    );
    pub fn CFRunLoopObserverCreate(
        allocator: CFAllocatorRef,
        activities: CFOptionFlags,
        repeats: Boolean,
        order: CFIndex,
        callout: CFRunLoopObserverCallBack,
        context: *mut CFRunLoopObserverContext,
    ) -> CFRunLoopObserverRef;
}

// `"C-unwind"`: running the loop runs its callouts — sources, observers, timers
// and the main queue's blocks — so an Objective-C exception one of them lets
// escape unwinds through this call. Declared `"C"`, that unwind would cross a
// frame rustc compiled as `nounwind`, skipping its destructors or aborting.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C-unwind" {
    pub fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: CFTimeInterval,
        return_after_source_handled: Boolean,
    ) -> CFRunLoopRunResult;
}

/// A +1 `CFUUIDRef`: `Clone` retains, `Drop` releases.
pub struct CFUUID(NonNull<__CFUUID>);

// SAFETY: a CFUUID is immutable after creation, and CoreFoundation's
// retain/release are atomic, so the handle may move and be shared across threads.
unsafe impl Send for CFUUID {}
// SAFETY: as for `Send`.
unsafe impl Sync for CFUUID {}

impl CFUUID {
    /// Adopt a +1 reference (a `…Create…` return); `None` for NULL.
    ///
    /// # Safety
    /// `uuid` is NULL or a +1 `CFUUIDRef` whose reference the caller gives up.
    #[must_use]
    pub unsafe fn from_create(uuid: CFUUIDRef) -> Option<Self> {
        NonNull::new(uuid.cast_mut()).map(Self)
    }

    /// The `CFUUIDRef`, borrowed for as long as `self` lives.
    #[must_use]
    pub fn as_ptr(&self) -> CFUUIDRef {
        self.0.as_ptr()
    }

    /// The sixteen bytes, in `CFUUIDBytes` order.
    #[must_use]
    pub fn bytes(&self) -> [u8; 16] {
        // SAFETY: `self` holds a live CFUUID.
        let b = unsafe { CFUUIDGetUUIDBytes(self.as_ptr()) };
        [
            b.byte0, b.byte1, b.byte2, b.byte3, b.byte4, b.byte5, b.byte6, b.byte7, b.byte8,
            b.byte9, b.byte10, b.byte11, b.byte12, b.byte13, b.byte14, b.byte15,
        ]
    }
}

impl Clone for CFUUID {
    fn clone(&self) -> Self {
        // SAFETY: `self` holds a live CFUUID; the new +1 is the clone's.
        unsafe { CFRetain(self.as_ptr().cast()) };
        Self(self.0)
    }
}

impl Drop for CFUUID {
    fn drop(&mut self) {
        // SAFETY: the +1 this handle owns, released once.
        unsafe { CFRelease(self.as_ptr().cast()) };
    }
}

impl fmt::Debug for CFUUID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CFUUID").field(&self.bytes()).finish()
    }
}

/// A +1 `CFStringRef`, read out as a Rust string through `Display`.
pub struct CFString(NonNull<__CFString>);

impl CFString {
    /// Adopt a +1 reference (a `…Copy…`/`…Create…` return); `None` for NULL.
    ///
    /// # Safety
    /// `string` is NULL or a +1 `CFStringRef` whose reference the caller gives
    /// up.
    #[must_use]
    pub unsafe fn from_create(string: CFStringRef) -> Option<Self> {
        NonNull::new(string.cast_mut()).map(Self)
    }

    /// The `CFStringRef`, borrowed for as long as `self` lives.
    #[must_use]
    pub fn as_ptr(&self) -> CFStringRef {
        self.0.as_ptr()
    }

    /// The whole string, read as its UTF-16 code units in one copy. A lone
    /// surrogate, which no UTF-8 can carry, reads as U+FFFD.
    fn read(&self) -> String {
        let s = self.as_ptr();
        // SAFETY: `self` holds a live CFString.
        let length = unsafe { CFStringGetLength(s) };
        let mut units = vec![0_u16; usize::try_from(length).unwrap_or(0)];
        let range = CFRange {
            location: 0,
            length,
        };
        // SAFETY: `units` has room for the `length` code units of `range`,
        // which is the whole string.
        unsafe { CFStringGetCharacters(s, range, units.as_mut_ptr()) };
        String::from_utf16_lossy(&units)
    }
}

impl Drop for CFString {
    fn drop(&mut self) {
        // SAFETY: the +1 this handle owns, released once.
        unsafe { CFRelease(self.as_ptr().cast()) };
    }
}

impl fmt::Display for CFString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.read())
    }
}

impl fmt::Debug for CFString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.read())
    }
}
