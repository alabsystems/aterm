// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime basics: class lookup, NSString round trips, ownership, pools, nil sends, and the
//! selector cache.

#![cfg(target_os = "macos")]

use std::ffi::c_char;

use aterm_objc::{
    Id, Obj, Sel, autoreleasepool, class, class_name, class_of, msg, ns_string, ns_string_to_rust,
    sel, sel_uncached, superclass_of,
};

#[test]
fn classes_resolve_and_unknown_names_are_nil() {
    assert!(!class(c"NSString").is_null());
    assert!(!class(c"NSObject").is_null());
    assert!(class(c"ATermNoSuchClassExistsAnywhere").is_null());
    // SAFETY: the pointer came straight from `objc_getClass` for a class
    // Foundation always defines, so it is live and immortal.
    unsafe { assert_eq!(class_name(class(c"NSString")), c"NSString") };
}

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
fn ns_string_length_is_utf16_code_units() {
    autoreleasepool(|_| {
        let obj = ns_string("héllo").expect("NSString");
        // SAFETY: `-length` is `-(NSUInteger)` on a live NSString.
        let len: usize = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
            f(obj.id(), sel!(length))
        };
        assert_eq!(len, 5, "NSString length counts UTF-16 units, not bytes");
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
fn class_of_and_superclass_of_walk_the_chain() {
    autoreleasepool(|_| {
        let obj = ns_string("chain").expect("NSString");
        // SAFETY: `obj` is a live object.
        let cls = unsafe { class_of(obj.id()) };
        assert!(!cls.is_null());
        // NSString is class-clustered, so the concrete class is a private subclass;
        // walking up must still reach NSString and then NSObject.
        let mut cursor = cls;
        let mut names = Vec::new();
        while !cursor.is_null() {
            // SAFETY: `cursor` starts at a live class (from `object_getClass` on a
            // live object) and each step replaces it with that class's superclass,
            // which the runtime guarantees is live-or-null.
            unsafe {
                names.push(class_name(cursor).to_string_lossy().into_owned());
                cursor = superclass_of(cursor);
            }
        }
        assert!(
            names.contains(&"NSString".to_owned()),
            "chain was {names:?}"
        );
        assert_eq!(names.last().map(String::as_str), Some("NSObject"));
    });
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
fn cached_selectors_are_the_uncached_ones() {
    assert_eq!(sel!(length), sel_uncached(c"length"));
    assert_eq!(sel!(alloc), sel_uncached(c"alloc"));
    assert_eq!(
        sel!(initWithBytes:length:encoding:),
        sel_uncached(c"initWithBytes:length:encoding:")
    );
    assert_eq!(
        sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
        sel_uncached(c"toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:")
    );
    // A cached selector is stable across uses.
    assert_eq!(sel!(description), sel!(description));
}

#[test]
fn a_cache_slot_survives_concurrent_first_use() {
    // The fill path is deliberately racy-by-value: both threads call
    // `sel_registerName`, which interns, so both must see the same pointer.
    let slot = std::sync::Arc::new(aterm_objc::SelCache::new());
    let mut handles = Vec::new();
    for _ in 0..8 {
        let slot = std::sync::Arc::clone(&slot);
        handles.push(std::thread::spawn(move || {
            slot.get(c"isEqual:").as_ptr().addr()
        }));
    }
    let seen: Vec<usize> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    assert!(
        seen.iter().all(|a| *a == seen[0]),
        "racing fills disagreed: {seen:?}"
    );
    assert_eq!(seen[0], sel_uncached(c"isEqual:").as_ptr().addr());
}

#[test]
fn a_pool_scope_is_a_pure_nesting() {
    // Nested scopes pop in reverse order without disturbing each other; `pools.rs` covers the
    // out-of-order case.
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
fn an_autoreleased_object_lives_exactly_as_long_as_its_pool() {
    // `Obj::autorelease` hands the +1 to the pool: the object stays readable for the rest of the
    // scope. A plain NSObject, for the reason in `owned_and_retained_balance_exactly`.
    let count = |id: Id| -> usize {
        // SAFETY: `-retainCount` is `-(NSUInteger)` and diagnostic-only.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
            f(id, sel!(retainCount))
        }
    };
    autoreleasepool(|_| {
        // SAFETY: `+alloc`/`-init` on NSObject return a +1 instance.
        let owned = unsafe {
            let alloc: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel) -> Id = msg();
            let raw = alloc(class(c"NSObject"), sel!(alloc));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            Obj::from_owned(init(raw, sel!(init))).expect("NSObject instance")
        };
        let before = count(owned.id());
        // Hand the +1 to the pool; the pointer that comes back is borrowed.
        let borrowed = owned.autorelease();
        assert_eq!(
            count(borrowed),
            before,
            "autorelease must not change the retain count — it defers a release"
        );
        // Still fully alive and messageable inside the pool.
        // SAFETY: `-description` is `-(id)` on the live object above.
        let desc = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            f(borrowed, sel!(description))
        };
        assert!(!desc.is_null());
    });
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

/// The premise of `send_rect`'s nil check, on x86_64: a nil `objc_msgSend_stret` leaves its
/// out-buffer unwritten. The stret ABI passes the result pointer first, so
/// `(out, self, _cmd) -> ()` is the same call with a buffer the test can poison.
#[cfg(target_arch = "x86_64")]
#[test]
fn libobjc_leaves_a_nil_stret_return_untouched_on_x86_64() {
    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_msgSend_stret();
    }
    let mut out = [0xABu8; size_of::<aterm_objc::CGRect>()];
    // SAFETY: on x86_64 `objc_msgSend_stret` takes the result pointer in rdi,
    // self in rsi and _cmd in rdx, which is exactly this prototype; a nil self
    // returns at once, and `out` is large enough for the CGRect `-frame` would
    // return.
    unsafe {
        let f: unsafe extern "C" fn(*mut u8, aterm_objc::Id, aterm_objc::Sel) =
            std::mem::transmute(objc_msgSend_stret as unsafe extern "C" fn());
        f(
            out.as_mut_ptr(),
            aterm_objc::Id::NIL,
            aterm_objc::sel!(frame),
        );
    }
    assert!(
        out.iter().all(|&b| b == 0xAB),
        "libobjc wrote the out-buffer of a nil stret send ({out:02x?}); `send_rect`'s \
         nil check would then be redundant on x86_64"
    );
}
