// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_objc::cf`'s two owners AT RUN TIME, on CoreFoundation alone.
//!
//! `cf_sdk.rs` proves the declarations are the headers'; this proves the
//! owners balance their retains and the string reads out whole. `aterm-cg`
//! has no run-time test: every CoreGraphics display call opens a WindowServer
//! connection, so only a real aterm exercises it.

#![cfg(target_os = "macos")]

use std::ptr;

use aterm_objc::cf::{
    CFAllocatorRef, CFIndex, CFString, CFStringRef, CFTypeRef, CFUUID, CFUUIDBytes, CFUUIDRef,
};

unsafe extern "C" {
    fn CFGetRetainCount(cf: CFTypeRef) -> CFIndex;
    fn CFUUIDCreateFromUUIDBytes(alloc: CFAllocatorRef, bytes: CFUUIDBytes) -> CFUUIDRef;
    fn CFStringCreateWithCharacters(
        alloc: CFAllocatorRef,
        chars: *const u16,
        num_chars: CFIndex,
    ) -> CFStringRef;
}

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
        // `IO32BitDirectPixels`, the string upstream winit's macOS `video_modes` compares.
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
