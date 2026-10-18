// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime basics: NSString round trips, ownership, pools, and nil sends.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::ffi::c_char;
use std::sync::Arc;

use aterm_objc::{Id, Obj, Sel, autoreleasepool, class, msg, ns_string, ns_string_to_rust, sel};

/// Every Rust string survives the trip exactly, including the two characters a
/// C-string or byte-order-mark reading loses: U+0000 (a pasted `a\0b` read
/// back as `a`) and a leading U+FEFF (which `initWithBytes:length:encoding:`
/// strips as a BOM).
#[test]
fn ns_string_round_trips_every_rust_string_exactly() {
    autoreleasepool(|_| {
        for s in [
            "",
            "hello",
            "héllo wörld",
            "🌊 kitty ⬆️",
            "line\nbreak",
            "a\0b",
            "\0",
            "\u{FEFF}leading",
            "\u{FEFF}",
        ] {
            let obj = ns_string(s).expect("NSString from &str");
            // SAFETY: `-length` is `-(NSUInteger)` on a live NSString.
            let len: usize = unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
                f(obj.id(), sel!(length))
            };
            assert_eq!(len, s.encode_utf16().count(), "NSString built from {s:?}");
            // SAFETY: `obj` holds a live +1 NSString.
            assert_eq!(unsafe { ns_string_to_rust(obj.id()) }, s);
        }
    });
}

/// An `NSString` can hold an unpaired surrogate (a pasteboard or text-input
/// string cut mid-pair), which no Rust `String` can: it reads back as U+FFFD
/// with the rest of the text intact, not as an empty string.
#[test]
fn an_unpaired_surrogate_reads_as_a_replacement_character() {
    autoreleasepool(|_| {
        let units: [u16; 4] = [0x61, 0xD83C, 0x62, 0xDF0A];
        // SAFETY: `+alloc` returns a +1 instance that
        // `-initWithCharacters:length:` (`@32@0:8r^S16Q24`) consumes, reading
        // `units.len()` code units during the call and returning +1 or nil.
        let obj = unsafe {
            let alloc: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel) -> Id = msg();
            let raw = alloc(class(c"NSString"), sel!(alloc));
            let init: unsafe extern "C-unwind" fn(Id, Sel, *const u16, usize) -> Id = msg();
            Obj::from_owned(init(
                raw,
                sel!(initWithCharacters:length:),
                units.as_ptr(),
                units.len(),
            ))
        }
        .expect("NSString from UTF-16");
        // SAFETY: `obj` holds a live +1 NSString.
        assert_eq!(unsafe { ns_string_to_rust(obj.id()) }, "a\u{FFFD}b\u{FFFD}");
    });
}

#[test]
fn owned_and_retained_balance_exactly() {
    autoreleasepool(|_| {
        // A plain NSObject: a short NSString is an immortal tagged pointer whose `-retainCount`
        // is `UINT64_MAX`, and the arithmetic below would overflow.
        // SAFETY: `+alloc`/`-init` on NSObject return a +1 instance this `Obj`
        // adopts and releases exactly once.
        let obj = unsafe {
            let alloc: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel) -> Id = msg();
            let raw = alloc(class(c"NSObject"), sel!(alloc));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            Obj::from_owned(init(raw, sel!(init))).expect("NSObject instance")
        };
        // SAFETY: `-retainCount` is `-(NSUInteger)` and diagnostic-only.
        let count = |o: &Obj| -> usize {
            unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
                f(o.id(), sel!(retainCount))
            }
        };
        let before = count(&obj);
        let second = obj.clone_retained();
        assert_eq!(count(&obj), before + 1, "clone_retained did not retain");
        drop(second);
        assert_eq!(count(&obj), before, "drop did not release");
    });
}

#[test]
fn from_owned_and_retain_reject_nil() {
    // SAFETY: null is the one value both constructors are documented to accept
    // and to map to `None`.
    unsafe {
        assert!(Obj::from_owned(Id::NIL).is_none());
        assert!(Obj::retain(Id::NIL).is_none());
    }
}

#[test]
fn an_error_description_is_read_out_of_a_real_nserror() {
    autoreleasepool(|_| {
        // Build a real NSError rather than asserting on the nil path only.
        let domain = ns_string("ATermTestDomain").expect("NSString");
        // SAFETY: `+errorWithDomain:code:userInfo:` is a documented Foundation
        // constructor returning an autoreleased NSError; the pool above owns it.
        let err: Id = unsafe {
            let f: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel, Id, isize, Id) -> Id =
                msg();
            f(
                class(c"NSError"),
                sel!(errorWithDomain:code:userInfo:),
                domain.id(),
                42,
                Id::NIL,
            )
        };
        assert!(!err.is_null());
        // SAFETY: `err` is the live, autoreleased NSError built above; the pool
        // opened at the top of this test outlives the read.
        let text = unsafe { aterm_objc::ns_error_string(err) };
        assert!(
            text.contains("ATermTestDomain") && text.contains("42"),
            "localizedDescription was {text:?}"
        );
        // SAFETY: null is the documented nil input.
        assert_eq!(
            unsafe { aterm_objc::ns_error_string(Id::NIL) },
            "(nil NSError)"
        );
    });
}

#[test]
fn a_pool_scope_is_a_pure_nesting() {
    // Nested scopes pop in reverse order without disturbing each other; scopes are the only way
    // this crate lets a second pool exist without `unsafe`.
    let text = autoreleasepool(|_outer| {
        autoreleasepool(|_inner| {
            let s = ns_string("nested").expect("NSString");
            // SAFETY: `s` is a live +1 NSString owned by this scope.
            unsafe { ns_string_to_rust(s.id()) }
        })
    });
    assert_eq!(text, "nested");
}

#[test]
fn a_scope_pops_its_pool_even_when_the_body_unwinds() {
    // The pool is a local of `autoreleasepool`'s frame, so unwinding pops it. A skipped pop would
    // make the next pool on this thread pop against a stack the runtime disagrees with.
    let caught = std::panic::catch_unwind(|| {
        autoreleasepool(|_| {
            let s = ns_string("about to unwind").expect("NSString");
            let _ = s;
            panic!("deliberate");
        })
    });
    assert!(caught.is_err());
    // A pool pushed after the unwind still pops cleanly; a leaked token would kill the binary here.
    let after = autoreleasepool(|_| {
        let s = ns_string("after").expect("NSString");
        // SAFETY: `s` is a live +1 NSString owned by this scope.
        unsafe { ns_string_to_rust(s.id()) }
    });
    assert_eq!(after, "after");
}

#[test]
fn utf8_string_of_a_nil_receiver_is_empty() {
    // nil is a legal receiver and a send to it returns zero, which is why `Id` is a raw pointer,
    // not a `NonNull`.
    // SAFETY: sending to nil is defined by the runtime and returns 0/null.
    let p: *const c_char = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> *const c_char = msg();
        f(Id::NIL, sel!(UTF8String))
    };
    assert!(p.is_null());
    // SAFETY: null is the documented `None` input.
    assert_eq!(unsafe { ns_string_to_rust(Id::NIL) }, "");
}

/// Fills the stack depth the next call at this level will occupy with 0xAB.
#[inline(never)]
fn poison_the_next_frame() -> u64 {
    let junk = [0xABu8; 1024];
    std::hint::black_box(&junk);
    junk.iter().map(|&b| u64::from(b)).sum()
}

/// `send_rect` on nil with the result held in this frame, so its return slot is stack
/// [`poison_the_next_frame`] just filled with 0xAB. Called from the test body, the slot would be
/// the test's own clean local and read zero by luck.
#[inline(never)]
fn nil_rect_in_a_poisoned_frame() -> [u64; 4] {
    // SAFETY: `-frame` is `-(NSRect)frame`, and nil is a valid receiver.
    let r = unsafe { aterm_objc::send::send_rect(aterm_objc::Id::NIL, aterm_objc::sel!(frame)) };
    let r = std::hint::black_box(r);
    [
        r.origin.x.to_bits(),
        r.origin.y.to_bits(),
        r.size.width.to_bits(),
        r.size.height.to_bits(),
    ]
}

/// A nil receiver on `send_rect` returns a zero rect on both arches. A 32-byte CGRect is an
/// indirect (`objc_msgSend_stret`) return on x86_64, which libobjc's nil path leaves unwritten,
/// and register-class (d0-d3) on arm64, which it zeroes.
#[test]
fn a_nil_receiver_returns_a_zero_rect_on_this_arch() {
    for _ in 0..8 {
        let _ = std::hint::black_box(poison_the_next_frame());
        let bits = nil_rect_in_a_poisoned_frame();
        assert_eq!(bits, [0; 4], "a nil send_rect returned {bits:#018x?}");
    }
}

// ---------------------------------------------------------------------------
// The selector cache under a hard concurrent first use.
// ---------------------------------------------------------------------------

#[test]
fn the_sel_cache_is_correct_under_a_hard_concurrent_first_use() {
    // Many threads, many distinct slots, every first use racing: the relaxed
    // store must be benign by value, every thread seeing the same interned
    // pointer, equal to the uncached lookup.
    const THREADS: usize = 16;
    let barrier = Arc::new(std::sync::Barrier::new(THREADS));
    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let b = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            b.wait();
            let mut v = Vec::new();
            for _ in 0..64 {
                v.push(sel!(objectAtIndex:).as_ptr().addr());
                v.push(sel!(initWithBytes:length:encoding:).as_ptr().addr());
                v.push(sel!(description).as_ptr().addr());
                v.push(sel!(isEqualToString:).as_ptr().addr());
            }
            v
        }));
    }
    let all: Vec<Vec<usize>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = &all[0];
    for (i, v) in all.iter().enumerate() {
        assert_eq!(v, first, "thread {i} saw different selector pointers");
    }
    assert_eq!(
        first[0],
        aterm_objc::sel_uncached(c"objectAtIndex:").as_ptr().addr()
    );
    assert_eq!(
        first[1],
        aterm_objc::sel_uncached(c"initWithBytes:length:encoding:")
            .as_ptr()
            .addr()
    );
}
