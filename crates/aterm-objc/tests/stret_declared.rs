// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W3 P1 — the DECLARED side of an indirect struct return, driven.
//!
//! # The gap this file closes
//!
//! W1 bound the SEND side of the indirect return: [`aterm_objc::msg`] reads the
//! return type off the prototype and picks `objc_msgSend_stret` in a `const`
//! block, and `tests/abi.rs` proves the threshold. The DECLARE side had ZERO
//! evidence. Every method W2 moved onto `declare_class!` returns `void`, `BOOL`
//! or `id`; `drawRect:` TAKES a `CGRect` and returns nothing. So no trampoline
//! this crate has ever registered returned a struct by value, and the first one
//! that will is `vendor/winit`'s
//! `-firstRectForCharacterRange:actualRange:` — a 32-byte `NSRect` return
//! beside a by-value `NSRange` and an `NSRange *` out-parameter.
//!
//! # The premise this file REFUTES
//!
//! The precondition was written as "a 32-byte by-value return that is
//! `objc_msgSend_stret` on x86_64", i.e. as an x86_64-only shape that this box
//! could only codegen-prove. It is not. AAPCS64 returns a HOMOGENEOUS
//! FLOATING-POINT AGGREGATE of up to four members in `v0`–`v3`, and `NSRect` is
//! four `double`s — so `firstRectForCharacterRange:` is a REGISTER return on
//! arm64 and never touches the indirect path at all. The indirect path on
//! arm64 is reached by a >16-byte aggregate that is NOT an HFA, and the
//! indirect-result pointer lives in `x8`. MEASURED with clang on this box,
//! `-O1 -S`, reading the IMP's own prologue:
//!
//! ```text
//! IMP                       arm64                         x86_64
//! -(NSRect)rectFor:out:     d0-d3, args x0..x4            RDI=sret->RAX, args RSI..R9
//! -(Triple{qqq})tripleFor:  x8 = result ptr, args x0..x3  RDI=sret->RAX, args RSI..R8
//! -(NSRange)markedRange     x0/x1                         RAX/RDX
//! ```
//!
//! So this file drives THREE return classes on arm64 — HFA (`NSRect`),
//! INDIRECT (`Triple`, 24 bytes of `long`), and two-register (`NSRange`) — and
//! the indirect one is EXECUTED here, not merely codegen-proved. The x86_64
//! half of each row is codegen-proved, as everything about that slice is: this
//! box cannot execute it.
//!
//! # And it is driven REFLECTIVELY, which is the point
//!
//! A direct typed send proves the trampoline and the caller agree about
//! registers. It says nothing about the ENCODING STRING, because a send carries
//! none. `NSMethodSignature` is where the string is read, so every row here is
//! also driven through `NSInvocation`: Foundation parses what
//! `class_addMethod` was handed, computes `methodReturnLength` from it, lays
//! out the frame and calls the IMP. A lie in the encoding shows up there and
//! nowhere else — which is exactly the shape of the defect P1 exists to
//! prevent.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};

use aterm_objc::{
    CGPoint, CGRect, CGSize, ClassPtr, ClassType, Encode, Id, NSRange, Sel, class, declare_class,
    method_types, msg, sel, strip_method_offsets,
};

/// The witness a libtest worker cannot legitimately mint.
///
/// Every class here is an `NSObject` subclass with `()` ivars that touches no
/// AppKit state and never leaves this worker.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: no main-thread affinity; the (empty) ivars are born and dropped
    // on this one thread.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

/// Twenty-four bytes of `long`: bigger than two eightbytes and NOT a
/// homogeneous floating-point aggregate, so it returns INDIRECTLY on both Apple
/// ABIs — `x8` on arm64, the hidden `RDI` pointer on x86_64.
///
/// This is the shape `NSRect` is NOT, and having both in one file is what makes
/// the arm64 evidence real rather than incidental.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Triple {
    a: i64,
    b: i64,
    c: i64,
}

// SAFETY: clang emits `{?=qqq}` for an anonymous `struct { long a, b, c; }` —
// MEASURED on this box. The struct tag is `?` because the C type is anonymous;
// a named tag would appear in its place and `NSMethodSignature` reads the
// FIELDS, not the tag. `#[repr(C)]` with three `i64`s matches the C layout
// exactly and nothing is packed.
unsafe impl Encode for Triple {
    const ENCODING: &'static str = "{?=qqq}";
}

declare_class! {
    /// The three return classes, on one class, with the argument shape the real
    /// `NSTextInputClient` row uses.
    struct StretProbe: NSObject {
        const NAME: &str = "ATermW3StretProbe";
        type Ivars = ();

        /// `vendor/winit`'s `view.rs:376` row, spelling for spelling: a
        /// by-value `NSRange`, an `NSRange *` out-parameter, a 32-byte
        /// `NSRect` return. The values returned are DERIVED from the argument
        /// so that a register shift cannot produce them by accident.
        @sel(firstRectForCharacterRange:actualRange:)
        fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> CGRect {
            if !actual.is_null() {
                // SAFETY: the caller (a direct send below, or `NSInvocation`)
                // passes either null or a pointer to a live, writable
                // `NSRange` it owns for the duration of the call — the same
                // contract `NSTextInputClient` states for `actualRange:`.
                unsafe { actual.write(range) };
            }
            CGRect {
                #[allow(clippy::cast_precision_loss)]
                origin: CGPoint {
                    x: range.location as f64,
                    y: range.length as f64,
                },
                size: CGSize {
                    width: 1.5,
                    height: 2.5,
                },
            }
        }

        /// The INDIRECT return on BOTH ABIs: `x8` on arm64, `sret` on x86_64.
        @sel(tripleForCharacterRange:)
        fn triple(&self, range: NSRange) -> Triple {
            Triple {
                a: i64::try_from(range.location).unwrap_or(-1),
                b: i64::try_from(range.length).unwrap_or(-1),
                c: 42,
            }
        }

        /// The control: sixteen bytes, two integer registers, no indirection
        /// anywhere. If the checks below cannot tell this apart from the two
        /// above, they are measuring nothing.
        @sel(markedRange)
        fn marked_range(&self) -> NSRange {
            NSRange {
                location: 7,
                length: 9,
            }
        }
    }
}

// ---------------------------------------------------------------- reflection

/// `+[NSObject instanceMethodSignatureForSelector:]`, +0.
fn signature_for(cls: ClassPtr, s: Sel) -> Id {
    // SAFETY: `+instanceMethodSignatureForSelector:` is `-(id)(Class, SEL)` on
    // NSObject and returns an AUTORELEASED `NSMethodSignature` (+0), borrowed
    // here for the length of the enclosing pool.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
        f(cls.as_id(), sel!(instanceMethodSignatureForSelector:), s)
    }
}

/// `-[NSMethodSignature methodReturnLength]` — the byte count Foundation
/// computed FROM THE STRING `class_addMethod` was handed.
fn return_length(sig: Id) -> usize {
    // SAFETY: `-methodReturnLength` is `-(NSUInteger)` on a live signature.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        f(sig, sel!(methodReturnLength))
    }
}

/// `-[NSMethodSignature methodReturnType]` — the return's encoding, as
/// Foundation re-emits it.
fn return_type(sig: Id) -> String {
    // SAFETY: `-methodReturnType` is `-(const char *)` on a live signature;
    // the string is owned by the signature and outlives this read.
    let p = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
        f(sig, sel!(methodReturnType))
    };
    assert!(!p.is_null(), "a signature always reports a return type");
    // SAFETY: a non-null, NUL-terminated, signature-owned C string.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Build an `NSInvocation` for `sel` on `target`, +0.
fn invocation(target: Id, cls: ClassPtr, s: Sel) -> Id {
    let sig = signature_for(cls, s);
    assert!(!sig.is_null(), "the class implements the selector");
    // SAFETY: `+invocationWithMethodSignature:` is `-(id)(Class, SEL, id)` and
    // returns an autoreleased invocation; the two setters are `-(void)(id)` and
    // `-(void)(SEL)`.
    unsafe {
        let new: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
        let inv = new(
            class(c"NSInvocation").as_id(),
            sel!(invocationWithMethodSignature:),
            sig,
        );
        assert!(!inv.is_null(), "NSInvocation was built");
        let set_id: unsafe extern "C" fn(Id, Sel, Id) = msg();
        set_id(inv, sel!(setTarget:), target);
        let set_sel: unsafe extern "C" fn(Id, Sel, Sel) = msg();
        set_sel(inv, sel!(setSelector:), s);
        inv
    }
}

/// `-[NSInvocation setArgument:atIndex:]`. Index 0 is `self`, 1 is `_cmd`.
///
/// # Safety
/// `slot` must point at a value of exactly the type the signature records for
/// `index`; `NSInvocation` copies that many bytes out of it.
unsafe fn set_argument(inv: Id, slot: *const c_void, index: isize) {
    // SAFETY: the caller pins `slot`'s type and the prototype is
    // `-(void)(void *, NSInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const c_void, isize) = msg();
        f(inv, sel!(setArgument:atIndex:), slot, index);
    }
}

/// `-invoke`, then `-getReturnValue:` into a `T`.
///
/// # Safety
/// `T` must be exactly the return type the signature records, or Foundation
/// writes `methodReturnLength` bytes into too small a slot.
unsafe fn invoke_returning<T: Default>(inv: Id) -> T {
    let mut out = T::default();
    // SAFETY: `-invoke` is `-(void)`; `-getReturnValue:` is `-(void)(void *)`
    // and writes exactly `methodReturnLength` bytes, which the caller has
    // pinned to `size_of::<T>()`.
    unsafe {
        let go: unsafe extern "C" fn(Id, Sel) = msg();
        go(inv, sel!(invoke));
        let get: unsafe extern "C" fn(Id, Sel, *mut c_void) = msg();
        get(
            inv,
            sel!(getReturnValue:),
            std::ptr::from_mut(&mut out).cast(),
        );
    }
    out
}

// --------------------------------------------------------------------- tests

/// What the runtime holds for each row is the offset-free form of what clang
/// emits for the same C signature — measured against the compiler, not asserted.
#[test]
fn the_registered_encodings_are_what_clang_emits_for_the_same_signatures() {
    let cls = StretProbe::class();

    // MEASURED with `clang -fobjc-arc -framework Foundation`, reading
    // `method_getTypeEncoding` off a compiler-built class with these exact
    // signatures (`scratch/w3o/ret.m`):
    //
    //   -(NSRect)rectFor:(NSRange)r out:(NSRangePointer)o
    //     {CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32
    //   -(Triple)tripleFor:(NSRange)r     {?=qqq}32@0:8{_NSRange=QQ}16
    //   -(NSRange)markedRange             {_NSRange=QQ}16@0:8
    for (s, clang_emitted) in [
        (
            sel!(firstRectForCharacterRange:actualRange:),
            "{CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32",
        ),
        (
            sel!(tripleForCharacterRange:),
            "{?=qqq}32@0:8{_NSRange=QQ}16",
        ),
        (sel!(markedRange), "{_NSRange=QQ}16@0:8"),
    ] {
        // SAFETY: `cls` is the live class `class()` just registered.
        let registered = unsafe { method_types(cls, s) }.expect("the method is registered");
        assert_eq!(
            registered,
            strip_method_offsets(clang_emitted),
            "registered encoding for {s:?} is not clang's, modulo offsets"
        );
    }
}

/// Foundation reads OUR string and gets the right byte count — 32, 24 and 16.
///
/// This is the assertion the encoding contract actually rests on. A send would
/// pass whatever the string said; `NSMethodSignature` is where it is believed.
#[test]
fn foundation_computes_the_return_length_from_the_registered_string() {
    let cls = StretProbe::class();
    for (s, want_len, want_type) in [
        (
            sel!(firstRectForCharacterRange:actualRange:),
            32_usize,
            "{CGRect={CGPoint=dd}{CGSize=dd}}",
        ),
        (sel!(tripleForCharacterRange:), 24, "{?=qqq}"),
        (sel!(markedRange), 16, "{_NSRange=QQ}"),
    ] {
        let sig = signature_for(cls, s);
        assert!(!sig.is_null(), "{s:?} has a signature");
        assert_eq!(return_length(sig), want_len, "return length for {s:?}");
        assert_eq!(return_type(sig), want_type, "return type for {s:?}");
    }
    // The three lengths are DIFFERENT, so the check discriminates.
    assert_eq!(size_of::<CGRect>(), 32);
    assert_eq!(size_of::<Triple>(), 24);
    assert_eq!(size_of::<NSRange>(), 16);
}

/// A direct, typed `objc_msgSend` into each declared trampoline: the register
/// contract, executed.
#[test]
fn a_direct_send_reaches_each_return_class_with_its_arguments_intact() {
    let obj = StretProbe::alloc_init(mtm(), ()).expect("the probe instantiates");
    let this = obj.as_id();

    // --- HFA(4) on arm64, `sret` on x86_64, with the full argument shape.
    let mut actual = NSRange::default();
    let rect = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRange, *mut NSRange) -> CGRect = msg();
        f(
            this,
            sel!(firstRectForCharacterRange:actualRange:),
            NSRange {
                location: 11,
                length: 22,
            },
            &raw mut actual,
        )
    };
    assert_eq!(
        rect,
        CGRect {
            origin: CGPoint { x: 11.0, y: 22.0 },
            size: CGSize {
                width: 1.5,
                height: 2.5
            },
        },
        "all four doubles of the 32-byte return arrived, derived from the \
         by-value NSRange"
    );
    assert_eq!(
        actual,
        NSRange {
            location: 11,
            length: 22
        },
        "the NSRange* out-parameter is the LAST argument register and would be \
         the first casualty of a hidden-result-pointer shift"
    );

    // --- INDIRECT on both ABIs.
    let triple = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRange) -> Triple = msg();
        f(
            this,
            sel!(tripleForCharacterRange:),
            NSRange {
                location: 5,
                length: 6,
            },
        )
    };
    assert_eq!(triple, Triple { a: 5, b: 6, c: 42 });

    // --- two registers, no indirection.
    let marked = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRange = msg();
        f(this, sel!(markedRange))
    };
    assert_eq!(
        marked,
        NSRange {
            location: 7,
            length: 9
        }
    );

    // The SEND side agrees with the crate's own model of this target: on arm64
    // nothing is indirect (the result pointer is `x8`, which no argument
    // occupies, so one entry point serves everything); on the x86_64 compat
    // slice the first two rows are.
    assert_eq!(
        aterm_objc::returns_indirectly::<CGRect>(),
        cfg!(target_arch = "x86_64")
    );
    assert_eq!(
        aterm_objc::returns_indirectly::<Triple>(),
        cfg!(target_arch = "x86_64")
    );
    assert!(!aterm_objc::returns_indirectly::<NSRange>());
}

/// The REFLECTIVE drive: Foundation lays out the frame from the registered
/// string and calls the trampoline itself.
///
/// This is the path a direct send cannot exercise and the one that reads the
/// encoding. `NSInvocation` allocates `methodReturnLength` bytes, calls the IMP
/// through the ABI the string implies, and copies the result out — so a
/// declared indirect return that the string did not describe corrupts here and
/// only here.
#[test]
fn nsinvocation_drives_every_return_class_through_the_registered_encoding() {
    aterm_objc::autoreleasepool(|_| {
        let obj = StretProbe::alloc_init(mtm(), ()).expect("the probe instantiates");
        let this = obj.as_id();
        let cls = StretProbe::class();

        // --- 32-byte return, by-value NSRange, NSRange* out-parameter.
        let s = sel!(firstRectForCharacterRange:actualRange:);
        let inv = invocation(this, cls, s);
        let range = NSRange {
            location: 101,
            length: 202,
        };
        let mut actual = NSRange::default();
        let mut actual_ptr: *mut NSRange = &raw mut actual;
        // SAFETY: index 2 is the `NSRange` and index 3 the `NSRange *`, which
        // is what the signature records; the slots hold exactly those types.
        unsafe {
            set_argument(inv, std::ptr::from_ref(&range).cast(), 2);
            set_argument(inv, std::ptr::from_mut(&mut actual_ptr).cast(), 3);
        }
        // SAFETY: `CGRect` is the return type the signature records, checked by
        // `foundation_computes_the_return_length_from_the_registered_string`.
        let rect: CGRect = unsafe { invoke_returning(inv) };
        assert_eq!(
            rect,
            CGRect {
                origin: CGPoint { x: 101.0, y: 202.0 },
                size: CGSize {
                    width: 1.5,
                    height: 2.5
                },
            },
            "NSInvocation read the 32-byte return through the encoding we \
             registered"
        );
        assert_eq!(
            actual,
            NSRange {
                location: 101,
                length: 202
            },
            "and the out-parameter survived the reflective frame"
        );

        // --- the indirect-on-both-ABIs return.
        let s = sel!(tripleForCharacterRange:);
        let inv = invocation(this, cls, s);
        let range = NSRange {
            location: 8,
            length: 9,
        };
        // SAFETY: index 2 is the `NSRange` the signature records.
        unsafe { set_argument(inv, std::ptr::from_ref(&range).cast(), 2) };
        // SAFETY: `Triple` is the recorded return type, 24 bytes.
        let triple: Triple = unsafe { invoke_returning(inv) };
        assert_eq!(triple, Triple { a: 8, b: 9, c: 42 });

        // --- and the two-register control, so a "reads 24 bytes of zero"
        //     failure mode could not pass all three.
        let inv = invocation(this, cls, sel!(markedRange));
        // SAFETY: `NSRange` is the recorded return type, 16 bytes.
        let marked: NSRange = unsafe { invoke_returning(inv) };
        assert_eq!(
            marked,
            NSRange {
                location: 7,
                length: 9
            }
        );
    });
}
