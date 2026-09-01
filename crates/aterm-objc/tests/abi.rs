// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two ABI facts that differ between the arm64 slice and the x86_64 compat
//! slice `lipo`d into the same universal binary: what `BOOL` is, and which
//! `objc_msgSend` a struct return goes through.
//!
//! # What is proved here, and by what
//!
//! This box is arm64 and cannot execute the x86_64 slice ("bad CPU type"), so
//! the two halves of each claim have different evidence and the difference is
//! stated rather than blurred:
//!
//! * **arm64 — EXECUTION.** The tests below run. They do not hard-code `"B"`:
//!   they read the encoding the SYSTEM's own compiler emitted for a Foundation
//!   method and compare this crate's constant against it, so the assertion is
//!   whatever the running platform says and would fail on x86_64 if the `cfg`
//!   arms were wrong.
//! * **x86_64 — CODEGEN.** Measured with `clang -arch x86_64 -S` on this box:
//!   `@encode(BOOL)` is `"c"` and `__OBJC_BOOL_IS_BOOL` is `0`; a 32-byte
//!   struct return compiles to `callq _objc_msgSend_stret` while a 16-byte one
//!   compiles to plain `_objc_msgSend`. The Rust side's x86_64 arms are
//!   type-checked but NOT executed here.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};

use aterm_objc::{
    Bool, CGPoint, CGRect, ClassPtr, Encode, Id, Sel, class, msg, returns_indirectly,
};

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
}

/// The type encoding the SYSTEM compiler emitted for `cls`'s `name`, or `None`
/// if that class does not implement it.
fn platform_encoding(cls: &'static CStr, name: &'static CStr) -> Option<String> {
    let cls = class(cls);
    assert!(!cls.is_null(), "{cls:?} is not linked into this binary");
    // SAFETY: `cls` is a live, immortal class object and the two calls are
    // side-effect-free runtime queries; the string they return is owned by the
    // runtime and outlives the read.
    unsafe {
        let m = class_getInstanceMethod(cls, aterm_objc::sel_uncached(name));
        if m.is_null() {
            return None;
        }
        let e = method_getTypeEncoding(m);
        if e.is_null() {
            return None;
        }
        Some(CStr::from_ptr(e).to_string_lossy().into_owned())
    }
}

#[test]
fn bool_encoding_matches_what_foundation_itself_emits() {
    // `-[NSObject isEqual:]` and `-[NSObject respondsToSelector:]` both return
    // `BOOL`, and their encodings were written by the compiler that built
    // Foundation for THIS architecture. Compiler-emitted encodings carry byte
    // offsets ("B24@0:8@16"), so only the leading return-type letter is
    // compared — which is the whole claim.
    let mut checked = 0;
    for sel in [c"isEqual:", c"respondsToSelector:", c"isProxy"] {
        let Some(enc) = platform_encoding(c"NSObject", sel) else {
            continue;
        };
        let leading = &enc[..1];
        assert_eq!(
            leading,
            Bool::ENCODING,
            "Foundation encodes -[NSObject {sel:?}]'s BOOL return as {leading:?} \
             but this crate says {:?} (full encoding {enc:?})",
            Bool::ENCODING
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no BOOL-returning NSObject method was readable"
    );

    // The value side of the same fact: one byte on both arches, and the
    // conversion is `!= 0` rather than a transmute, so no bit pattern is
    // invalid. On x86_64 `BOOL` is `signed char` and a sender may legally pass
    // any non-zero byte for YES; a Rust `bool` there would be UB.
    assert_eq!(size_of::<Bool>(), 1);
    assert!(Bool::YES.as_bool());
    assert!(!Bool::NO.as_bool());
    assert_eq!(Bool::default(), Bool::NO);
    assert_eq!(Bool::from(true), Bool::YES);
    assert!(bool::from(Bool::YES));
    assert_eq!(
        Bool::ENCODING,
        if cfg!(target_arch = "aarch64") {
            "B"
        } else {
            "c"
        },
        "the cfg arms and the measurement disagree"
    );
}

#[test]
fn the_struct_encodings_match_what_foundation_itself_emits() {
    // Same technique against `CGRect`: `-[NSValue rectValue]` is compiled by
    // the system compiler, so its encoding is the platform's own spelling of
    // the layout this crate hard-codes.
    let Some(enc) = platform_encoding(c"NSValue", c"rectValue") else {
        return;
    };
    assert!(
        enc.starts_with(CGRect::ENCODING),
        "Foundation spells NSRect {enc:?}; this crate says {:?}",
        CGRect::ENCODING
    );
}

#[test]
fn the_indirect_return_rule_matches_the_c_abi() {
    // MEASURED with clang, `-arch x86_64 -S -O1`, one selector per return type:
    //
    //   struct of 2 long   (16 bytes) -> _objc_msgSend
    //   struct of 2 double (16 bytes) -> _objc_msgSend
    //   char[17]           (17 bytes) -> _objc_msgSend_stret
    //   struct of 3 double (24 bytes) -> _objc_msgSend_stret
    //   NSRect             (32 bytes) -> _objc_msgSend_stret
    //   float / double                -> _objc_msgSend
    //   long double                   -> _objc_msgSend_fpret  (not expressible
    //                                    in Rust, so not bound)
    //
    // The threshold is therefore exactly "larger than 16 bytes", and on arm64
    // there is no threshold at all because the indirect-result pointer has its
    // own register.
    let x86 = cfg!(target_arch = "x86_64");
    assert_eq!(returns_indirectly::<CGRect>(), x86, "CGRect is 32 bytes");
    assert_eq!(returns_indirectly::<[u8; 17]>(), x86, "17 bytes");
    assert_eq!(returns_indirectly::<[u8; 24]>(), x86, "24 bytes");
    assert!(!returns_indirectly::<CGPoint>(), "CGPoint is 16 bytes");
    assert!(!returns_indirectly::<[u64; 2]>(), "16 bytes");
    assert!(!returns_indirectly::<f64>());
    assert!(!returns_indirectly::<Id>());
    assert!(!returns_indirectly::<Bool>());
    assert!(!returns_indirectly::<()>());
}

#[test]
fn the_entry_point_is_chosen_by_the_return_type() {
    // `msg` hands back a function pointer; which SYMBOL it points at is the
    // whole of D4. On x86_64 a >16-byte return must land on a different symbol
    // from an object return; on arm64 there is only one symbol, and asserting
    // that is what proves the crate is not smuggling a `_stret` reference into
    // a binary whose `libobjc` does not export one (that would be a load-time
    // failure in the shipped app, not a compile error here).
    //
    // Only the arm64 arm EXECUTES on this box; the x86_64 arm is type-checked.
    // SAFETY: neither pointer is called — the test only compares addresses.
    let (plain, big, point) = unsafe {
        let plain: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let big: unsafe extern "C" fn(Id, Sel) -> CGRect = msg();
        let point: unsafe extern "C" fn(Id, Sel) -> CGPoint = msg();
        (plain as usize, big as usize, point as usize)
    };
    if cfg!(target_arch = "x86_64") {
        assert_ne!(
            plain, big,
            "a 32-byte return must go through objc_msgSend_stret on x86_64"
        );
        assert_eq!(point, plain, "a 16-byte return stays on plain objc_msgSend");
    } else {
        assert_eq!(
            plain, big,
            "arm64 libobjc exports no _stret variant, so every return type must \
             resolve to the one entry point"
        );
        assert_eq!(point, plain);
    }
}
