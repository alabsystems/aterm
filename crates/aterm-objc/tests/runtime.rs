// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The promoted `metal/ffi.rs` surface, plus the selector cache.

#![cfg(target_os = "macos")]

use std::ffi::c_char;
use std::time::Instant;

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

#[test]
fn ns_string_round_trips_utf8_including_non_ascii() {
    autoreleasepool(|_| {
        for s in ["", "hello", "héllo wörld", "🌊 kitty ⬆️", "line\nbreak"] {
            let obj = ns_string(s).expect("NSString from &str");
            // SAFETY: `obj` holds a live +1 NSString.
            assert_eq!(unsafe { ns_string_to_rust(obj.id()) }, s);
        }
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
        // A plain NSObject, NOT an NSString: a short literal string becomes a
        // TAGGED POINTER, whose `-retainCount` is `UINT64_MAX` because it is
        // immortal. That is a real property of the runtime and the reason this test
        // does not use `ns_string` — the arithmetic below would overflow.
        // SAFETY: `+alloc`/`-init` on NSObject return a +1 instance this `Obj`
        // adopts and releases exactly once.
        let obj = unsafe {
            let alloc: unsafe extern "C-unwind" fn(aterm_objc::ClassPtr, Sel) -> Id = msg();
            let raw = alloc(class(c"NSObject"), sel!(alloc));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            Obj::from_owned(init(raw, sel!(init))).expect("NSObject instance")
        };
        // SAFETY: `-retainCount` is `-(NSUInteger)`; it is diagnostic-only, which
        // is precisely what this test wants it for.
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
    // Cached twice in a row is stable (the fill path ran once).
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

/// The measurement behind [`aterm_objc::sel!`]: the cached send path must not be
/// SLOWER than `metal/ffi.rs`'s uncached one.
///
/// Ignored by default because it is a timing test. Run it with
/// `cargo test -p aterm-objc --release -- --ignored --nocapture`.
///
/// Arms alternate ABBA within each round and the reported figure is the median
/// of the per-round ratios, because a single A-then-B pass on this box is
/// dominated by whichever arm warmed the caches.
#[test]
#[ignore = "timing measurement; run explicitly"]
fn cached_selectors_are_not_slower_than_uncached() {
    const ROUNDS: usize = 9;
    const ITERS: usize = 200_000;

    let s = ns_string("measurement").expect("NSString");
    let obj = s.id();

    // Each arm does the SAME work: resolve `-length`, send it, sum the answers.
    // The only difference is how the selector is resolved.
    let uncached = |obj: Id| -> (u128, usize) {
        let t = Instant::now();
        let mut acc = 0usize;
        for _ in 0..ITERS {
            // SAFETY: `-length` on a live NSString is `-(NSUInteger)`.
            acc += unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
                f(obj, sel_uncached(c"length"))
            };
        }
        (t.elapsed().as_nanos(), acc)
    };
    let cached = |obj: Id| -> (u128, usize) {
        let t = Instant::now();
        let mut acc = 0usize;
        for _ in 0..ITERS {
            // SAFETY: as above.
            acc += unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
                f(obj, sel!(length))
            };
        }
        (t.elapsed().as_nanos(), acc)
    };

    let mut ratios = Vec::new();
    for round in 0..ROUNDS {
        // ABBA within the round.
        let (a1, x1) = uncached(obj);
        let (b1, y1) = cached(obj);
        let (b2, y2) = cached(obj);
        let (a2, x2) = uncached(obj);
        assert_eq!(
            (x1, x2),
            (y1, y2),
            "the arms did not compute the same thing"
        );
        let un = (a1 + a2) as f64 / 2.0;
        let ca = (b1 + b2) as f64 / 2.0;
        println!(
            "round {round}: uncached {:.1} ns/send, cached {:.1} ns/send, ratio {:.3}",
            un / ITERS as f64,
            ca / ITERS as f64,
            ca / un
        );
        ratios.push(ca / un);
    }
    ratios.sort_by(f64::total_cmp);
    let median = ratios[ROUNDS / 2];
    println!("median cached/uncached ratio: {median:.3}");
    assert!(
        median <= 1.0,
        "the cached send path is SLOWER than the uncached one (ratio {median:.3})"
    );
}

#[test]
fn a_pool_scope_is_a_pure_nesting() {
    // Nested pools must pop in reverse order without disturbing each other; an
    // autoreleased object created inside the inner pool must still be readable
    // while that pool is live. Written as nested SCOPES because that is now the
    // only way this crate lets a second pool exist — see `pools.rs` for what
    // the RAII-token form allowed.
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
    // The capability D5 named: `objc_autorelease`. An object handed to the pool
    // is readable for the rest of the scope and released when the scope ends.
    // Measured through `-retainCount` on a plain NSObject (NOT an NSString: a
    // short literal is a tagged pointer whose retain count is `UINT64_MAX`).
    let count = |id: Id| -> usize {
        // SAFETY: `-retainCount` is `-(NSUInteger)` and diagnostic-only, which
        // is exactly what this test wants it for.
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
    // nil is a legal receiver; a send to it returns zero. This is the property
    // `Id` is a raw pointer rather than a `NonNull` for.
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

/// `send_rect` on nil with the result held in THIS frame, so its struct-return
/// slot is stack [`poison_the_next_frame`] just filled with 0xAB. A call written
/// in the test body forwards the slot to the test's own local, whose bytes
/// nothing dirtied, and reads zero by luck; that is how the first cut of this
/// test passed on an Intel Mac with the guard deleted.
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

/// A nil receiver on a struct-returning send hands back ZERO — on x86_64 too.
///
/// libobjc zeroes register-class returns on the nil path and leaves an
/// INDIRECT (`objc_msgSend_stret`) return untouched (pinned below). A 32-byte
/// CGRect is indirect on x86_64 and register-class (d0-d3) on arm64, so without
/// `send_rect`'s own nil check it reads whatever the return slot held on an
/// Intel Mac and zero on every arm64 dev machine.
#[test]
fn a_nil_receiver_returns_a_zero_rect_on_this_arch() {
    for _ in 0..8 {
        let _ = std::hint::black_box(poison_the_next_frame());
        let bits = nil_rect_in_a_poisoned_frame();
        assert_eq!(bits, [0; 4], "a nil send_rect returned {bits:#018x?}");
    }
}

/// The premise the guard exists for, measured on the arch that has it: a nil
/// `objc_msgSend_stret` returns without writing its out-buffer. The x86_64
/// stret ABI passes the hidden result pointer first, so declaring the entry as
/// `(out, self, _cmd) -> ()` is the same call with the pointer made explicit —
/// and the buffer can be poisoned deterministically.
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
