// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The declared side of an indirect struct return, driven. The send side is
//! [`aterm_objc::msg`], which picks `objc_msgSend_stret` from the prototype's return type
//! (`tests/abi.rs` checks the threshold). This file covers `declare_class!` trampolines that
//! return a struct by value, such as `crates/aterm-window`'s
//! `-firstRectForCharacterRange:actualRange:`: a 32-byte `NSRect` return beside a by-value
//! `NSRange` and an `NSRange *` out-parameter.
//!
//! AAPCS64 returns a homogeneous floating-point aggregate of up to four members in `v0`–`v3`, so
//! `NSRect` is a register return on arm64; only a >16-byte non-HFA aggregate is indirect, through
//! `x8`. Measured with clang (`-O1 -S`, the IMP's prologue):
//!
//! ```text
//! IMP                       arm64                         x86_64
//! -(NSRect)rectFor:out:     d0-d3, args x0..x4            RDI=sret->RAX, args RSI..R9
//! -(Triple{qqq})tripleFor:  x8 = result ptr, args x0..x3  RDI=sret->RAX, args RSI..R8
//! -(NSRange)markedRange     x0/x1                         RAX/RDX
//! ```
//!
//! So the file executes three return classes on arm64: HFA (`NSRect`), indirect (`Triple`) and
//! two-register (`NSRange`); the x86_64 column is codegen-only.
//!
//! Each row is also driven through `NSInvocation`: a send carries no encoding string, but
//! Foundation lays out the frame from what `class_addMethod` was handed, so a lie in the encoding
//! shows up there and nowhere else.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "aterm-objc's own runtime tests drive the bare `msg` cast; everything else uses checked_msg"
)]

use std::ffi::c_void;

use aterm_objc::{
    CGPoint, CGRect, CGSize, ClassPtr, ClassType, Encode, Id, NSRange, Sel, class, declare_class,
    method_types, msg, sel, strip_method_offsets,
};

/// The witness, minted unchecked: libtest runs every test on a worker.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: no main-thread affinity; the (empty) ivars are born and dropped
    // on this one thread.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

/// Twenty-four bytes of `long`: over two eightbytes and not a homogeneous floating-point aggregate,
/// so unlike `NSRect` it returns indirectly on both Apple ABIs (`x8` on arm64, the hidden `RDI`
/// pointer on x86_64).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct Triple {
    a: i64,
    b: i64,
    c: i64,
}

// SAFETY: three `i64`s; all-zero is `{0, 0, 0}`, a valid `Triple`, which is what a contained
// `tripleForCharacterRange:` answers.
unsafe impl aterm_objc::InertZero for Triple {}

// SAFETY: clang emits `{?=qqq}` for an anonymous `struct { long a, b, c; }`; `NSMethodSignature`
// reads the fields, not the tag. `#[repr(C)]` with three `i64`s matches the C layout, unpacked.
unsafe impl Encode for Triple {
    const ENCODING: &'static str = "{?=qqq}";
}

declare_class! {
    /// The three return classes, on one class, with the argument shape the real
    /// `NSTextInputClient` row uses.
    struct StretProbe: NSObject {
        const NAME: &str = "ATermW3StretProbe";
        type Ivars = ();

        /// `crates/aterm-window`'s `view.rs` row, spelling for spelling: a by-value `NSRange`, an
        /// `NSRange *` out-parameter, a 32-byte `NSRect` return. The result derives from the
        /// argument, so a register shift cannot produce it by accident.
        @sel(firstRectForCharacterRange:actualRange:)
        fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> CGRect {
            if !actual.is_null() {
                // SAFETY: the caller (a direct send below, or `NSInvocation`) passes null or a
                // live, writable `NSRange` it owns for the call, as `NSTextInputClient` states
                // for `actualRange:`.
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

        /// The indirect return on both ABIs: `x8` on arm64, `sret` on x86_64.
        @sel(tripleForCharacterRange:)
        fn triple(&self, range: NSRange) -> Triple {
            Triple {
                a: i64::try_from(range.location).unwrap_or(-1),
                b: i64::try_from(range.length).unwrap_or(-1),
                c: 42,
            }
        }

        /// The control: sixteen bytes in two integer registers, no indirection.
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
    // SAFETY: `+instanceMethodSignatureForSelector:` is `-(id)(Class, SEL)` on NSObject and
    // returns an autoreleased `NSMethodSignature` (+0), borrowed for the enclosing pool.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = msg();
        f(cls.as_id(), sel!(instanceMethodSignatureForSelector:), s)
    }
}

/// Build an `NSInvocation` for `sel` on `target`, +0.
fn invocation(target: Id, cls: ClassPtr, s: Sel) -> Id {
    let sig = signature_for(cls, s);
    assert!(!sig.is_null(), "the class implements the selector");
    // SAFETY: `+invocationWithMethodSignature:` is `-(id)(Class, SEL, id)` and
    // returns an autoreleased invocation; the two setters are `-(void)(id)` and
    // `-(void)(SEL)`.
    unsafe {
        let new: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = msg();
        let inv = new(
            class(c"NSInvocation").as_id(),
            sel!(invocationWithMethodSignature:),
            sig,
        );
        assert!(!inv.is_null(), "NSInvocation was built");
        let set_id: unsafe extern "C-unwind" fn(Id, Sel, Id) = msg();
        set_id(inv, sel!(setTarget:), target);
        let set_sel: unsafe extern "C-unwind" fn(Id, Sel, Sel) = msg();
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
        let f: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, isize) = msg();
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
        let go: unsafe extern "C-unwind" fn(Id, Sel) = msg();
        go(inv, sel!(invoke));
        let get: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) = msg();
        get(
            inv,
            sel!(getReturnValue:),
            std::ptr::from_mut(&mut out).cast(),
        );
    }
    out
}

// --------------------------------------------------------------------- tests

/// Each row's registered encoding is clang's for the same C signature, minus offsets.
#[test]
fn the_registered_encodings_are_what_clang_emits_for_the_same_signatures() {
    let cls = StretProbe::class();

    // `method_getTypeEncoding` of a clang-built class (`-fobjc-arc -framework Foundation`) with
    // these signatures:
    //
    //   -(NSRect)rectFor:(NSRange)r out:(NSRangePointer)o
    //   -(Triple)tripleFor:(NSRange)r
    //   -(NSRange)markedRange
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

/// A direct, typed `objc_msgSend` into each declared trampoline: the register
/// contract, executed.
#[test]
fn a_direct_send_reaches_each_return_class_with_its_arguments_intact() {
    let obj = StretProbe::alloc_init(mtm(), ()).expect("the probe instantiates");
    let this = obj.as_id();

    // --- HFA(4) on arm64, `sret` on x86_64, with the full argument shape.
    let mut actual = NSRange::default();
    let rect = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, NSRange, *mut NSRange) -> CGRect = msg();
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

    // --- Indirect on both ABIs.
    let triple = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, NSRange) -> Triple = msg();
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
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> NSRange = msg();
        f(this, sel!(markedRange))
    };
    assert_eq!(
        marked,
        NSRange {
            location: 7,
            length: 9
        }
    );

    // `msg`'s model agrees: on arm64 no row needs `_stret` (the result pointer is `x8`, which no
    // argument occupies); on x86_64 the first two do.
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

/// The reflective drive: `NSInvocation` sizes the return from the registered string, calls the
/// IMP through the ABI the string implies and copies the result out, so a declared indirect
/// return the string misdescribes corrupts here and only here.
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
        // SAFETY: `CGRect` is the return type the signature records, as
        // `the_registered_encodings_are_what_clang_emits_for_the_same_signatures`
        // checks.
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

        // --- the two-register control, so a reads-zero failure cannot pass all three.
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
