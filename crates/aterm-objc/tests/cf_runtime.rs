// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_objc::cf` at run time, on CoreFoundation alone: the two owners, and a run-loop source
//! and observer registered the way winit's macOS backend registers them. `cf_cg_sdk.rs` checks
//! the declarations against the headers; this checks that the owners balance their retains, the
//! string reads out whole, and CoreFoundation calls the callouts through the declared types (the
//! observer's `"C-unwind"` one included). `cg` has no run-time test: every CoreGraphics display
//! call opens a WindowServer connection.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::cf::{
    CFAllocatorRef, CFIndex, CFRelease, CFRunLoopActivity, CFRunLoopAddObserver,
    CFRunLoopAddSource, CFRunLoopObserverContext, CFRunLoopObserverCreate, CFRunLoopObserverRef,
    CFRunLoopRunInMode, CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceInvalidate,
    CFRunLoopSourceSignal, CFString, CFStringRef, CFTypeRef, CFUUID, CFUUIDBytes, CFUUIDRef,
    kCFRunLoopAfterWaiting, kCFRunLoopBeforeWaiting, kCFRunLoopDefaultMode,
};

unsafe extern "C" {
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFGetRetainCount(cf: CFTypeRef) -> CFIndex;
    fn CFUUIDCreateFromUUIDBytes(alloc: CFAllocatorRef, bytes: CFUUIDBytes) -> CFUUIDRef;
    fn CFStringCreateWithCharacters(
        alloc: CFAllocatorRef,
        chars: *const u16,
        num_chars: CFIndex,
    ) -> CFStringRef;
}

/// `kCFRunLoopRunTimedOut` and `kCFRunLoopRunHandledSource`.
const RUN_TIMED_OUT: i32 = 3;
const RUN_HANDLED_SOURCE: i32 = 4;

fn retain_count(cf: CFTypeRef) -> CFIndex {
    // SAFETY: `cf` is a live CoreFoundation object for the caller's borrow.
    unsafe { CFGetRetainCount(cf) }
}

#[test]
fn the_uuid_owner_reads_its_bytes_and_balances_its_retains() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<CFUUID>();

    let want: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    let b = CFUUIDBytes {
        byte0: want[0],
        byte1: want[1],
        byte2: want[2],
        byte3: want[3],
        byte4: want[4],
        byte5: want[5],
        byte6: want[6],
        byte7: want[7],
        byte8: want[8],
        byte9: want[9],
        byte10: want[10],
        byte11: want[11],
        byte12: want[12],
        byte13: want[13],
        byte14: want[14],
        byte15: want[15],
    };
    // SAFETY: the default allocator and sixteen bytes; the +1 (Create rule) is
    // adopted by `from_create`.
    let uuid = unsafe { CFUUID::from_create(CFUUIDCreateFromUUIDBytes(ptr::null(), b)) }
        .expect("CFUUIDCreateFromUUIDBytes answered a CFUUID");
    assert_eq!(
        uuid.bytes(),
        want,
        "the bytes come back in CFUUIDBytes order"
    );

    let before = retain_count(uuid.as_ptr().cast());
    let twin = uuid.clone();
    assert_eq!(
        retain_count(uuid.as_ptr().cast()),
        before + 1,
        "Clone retains"
    );
    assert_eq!(twin.bytes(), want);
    drop(twin);
    assert_eq!(
        retain_count(uuid.as_ptr().cast()),
        before,
        "Drop releases once"
    );

    // SAFETY: NULL is the documented "nothing to adopt".
    assert!(unsafe { CFUUID::from_create(ptr::null()) }.is_none());
}

fn cf_string(units: &[u16]) -> CFString {
    let len = CFIndex::try_from(units.len()).expect("a short string");
    // SAFETY: `units` is `len` UTF-16 code units; CoreFoundation copies them,
    // and the +1 (Create rule) is adopted by `from_create`.
    unsafe {
        CFString::from_create(CFStringCreateWithCharacters(
            ptr::null(),
            units.as_ptr(),
            len,
        ))
    }
    .expect("CFStringCreateWithCharacters answered a CFString")
}

#[test]
fn the_string_owner_reads_out_the_whole_string() {
    for s in [
        "",
        // `IO32BitDirectPixels`, the string winit's `video_modes` compares.
        "--------RRRRRRRRGGGGGGGGBBBBBBBB",
        "é → 😀 (two bytes, three, four)",
    ] {
        let units: Vec<u16> = s.encode_utf16().collect();
        assert_eq!(cf_string(&units).to_string(), s);
    }
    // A lone surrogate has no UTF-8 form: it reads as U+FFFD, and the rest of
    // the string still comes back (a UTF-8 `CFStringGetBytes` stops there).
    let lone = [0xD800, u16::from(b'a')];
    assert_eq!(cf_string(&lone).to_string(), "\u{FFFD}a");
}

static PERFORMED: AtomicUsize = AtomicUsize::new(0);
static PERFORMED_WITH_INFO: AtomicUsize = AtomicUsize::new(0);
static BEFORE_WAITING: AtomicUsize = AtomicUsize::new(0);
static AFTER_WAITING: AtomicUsize = AtomicUsize::new(0);

/// The source's `perform`, at the declared `void (*)(void *)`. It only counts
/// (a panic here would abort): the assertions are the thread's.
extern "C" fn perform(info: *mut c_void) {
    let counter = if info.is_null() {
        &PERFORMED
    } else {
        &PERFORMED_WITH_INFO
    };
    counter.fetch_add(1, Ordering::SeqCst);
}

/// The observer's callout, at the declared `"C-unwind"` type: registered with
/// no transmute, as `observer.rs` registers winit's.
extern "C-unwind" fn observe(_: CFRunLoopObserverRef, activity: CFRunLoopActivity, _: *mut c_void) {
    match activity {
        a if a == kCFRunLoopBeforeWaiting => BEFORE_WAITING.fetch_add(1, Ordering::SeqCst),
        a if a == kCFRunLoopAfterWaiting => AFTER_WAITING.fetch_add(1, Ordering::SeqCst),
        _ => 0,
    };
}

/// One thread's own run loop, so nothing else sources it: a timed run with an
/// unsignalled source waits (both observer activities fire), and a signalled
/// source is handled exactly once.
#[test]
fn a_source_and_an_observer_run_through_the_declared_callouts() {
    std::thread::spawn(|| {
        // SAFETY: this thread's run loop, used only on this thread; every
        // context is copied by CoreFoundation at creation, and both +1s are
        // released before the thread (and its run loop) ends.
        unsafe {
            let rl = CFRunLoopGetCurrent().cast();
            let mut source_context = CFRunLoopSourceContext {
                version: 0,
                info: ptr::null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform,
            };
            let source = CFRunLoopSourceCreate(ptr::null(), 0, &raw mut source_context);
            assert!(!source.is_null());
            CFRunLoopAddSource(rl, source, kCFRunLoopDefaultMode);

            let mut observer_context = CFRunLoopObserverContext {
                version: 0,
                info: ptr::null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
            };
            let observer = CFRunLoopObserverCreate(
                ptr::null(),
                kCFRunLoopBeforeWaiting | kCFRunLoopAfterWaiting,
                1,
                0,
                observe,
                &raw mut observer_context,
            );
            assert!(!observer.is_null());
            CFRunLoopAddObserver(rl, observer, kCFRunLoopDefaultMode);

            // Nothing is signalled, so the loop waits out the short run.
            let timed = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, 1);
            assert_eq!(
                timed, RUN_TIMED_OUT,
                "an unsignalled source does not end the run"
            );
            assert_eq!(PERFORMED.load(Ordering::SeqCst), 0);
            assert!(
                BEFORE_WAITING.load(Ordering::SeqCst) >= 1,
                "BeforeWaiting fired"
            );
            assert!(
                AFTER_WAITING.load(Ordering::SeqCst) >= 1,
                "AfterWaiting fired"
            );

            // Signalled, it is handled; 60 s is a hang detector, not a budget.
            CFRunLoopSourceSignal(source);
            let handled = CFRunLoopRunInMode(kCFRunLoopDefaultMode, 60.0, 1);
            assert_eq!(handled, RUN_HANDLED_SOURCE);
            assert_eq!(PERFORMED.load(Ordering::SeqCst), 1, "perform ran once");
            assert_eq!(
                PERFORMED_WITH_INFO.load(Ordering::SeqCst),
                0,
                "perform got the context's NULL info back"
            );

            CFRunLoopSourceInvalidate(source);
            CFRelease(source.cast_const().cast());
            CFRelease(observer.cast_const().cast());
        }
    })
    .join()
    .expect("the run-loop thread");
}
