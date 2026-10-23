// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

fn mtm() -> aterm_objc::MainThread {
    // SAFETY: no main-thread affinity; the empty ivars live and drop on this thread.
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

// SAFETY: three `i64`s; all-zero is `{0, 0, 0}`, a valid `Triple`.
unsafe impl aterm_objc::InertZero for Triple {}

// SAFETY: `#[repr(C)]` with three `i64`s matches `{?=qqq}` unpacked.
unsafe impl Encode for Triple {
    const ENCODING: &'static str = "{?=qqq}";
}

declare_class! {
    struct StretProbe: NSObject {
        const NAME: &str = "ATermW3StretProbe";
        type Ivars = ();

        @sel(firstRectForCharacterRange:actualRange:)
        fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> CGRect {
            if !actual.is_null() {
                // SAFETY: caller passes null or a live writable `NSRange` for the call.
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

        @sel(tripleForCharacterRange:)
        fn triple(&self, range: NSRange) -> Triple {
            Triple {
                a: i64::try_from(range.location).unwrap_or(-1),
                b: i64::try_from(range.length).unwrap_or(-1),
                c: 42,
            }
        }

        @sel(markedRange)
        fn marked_range(&self) -> NSRange {
            NSRange {
                location: 7,
                length: 9,
            }
        }
    }
}

fn signature_for(cls: ClassPtr, s: Sel) -> Id {
    // SAFETY: `+instanceMethodSignatureForSelector:` is `-(id)(Class, SEL)` on NSObject and
    // returns an autoreleased `NSMethodSignature` (+0), borrowed for the enclosing pool.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = msg();
        f(cls.as_id(), sel!(instanceMethodSignatureForSelector:), s)
    }
}

fn invocation(target: Id, cls: ClassPtr, s: Sel) -> Id {
    let sig = signature_for(cls, s);
    assert!(!sig.is_null(), "the class implements the selector");
    // SAFETY: `+invocationWithMethodSignature:`, `-setTarget:`, and `-setSelector:` match ABI.
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

/// `-[NSInvocation setArgument:atIndex:]`.
///
/// # Safety
/// `slot` must point at a value of the type recorded for `index`.
unsafe fn set_argument(inv: Id, slot: *const c_void, index: isize) {
    // SAFETY: caller pins `slot`'s type and prototype is `-(void)(void *, NSInteger)`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, isize) = msg();
        f(inv, sel!(setArgument:atIndex:), slot, index);
    }
}

/// `-invoke`, then `-getReturnValue:` into a `T`.
///
/// # Safety
/// `T` must be the exact return type recorded in the signature.
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

#[test]
fn the_registered_encodings_are_what_clang_emits_for_the_same_signatures() {
    let cls = StretProbe::class();

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

#[test]
fn a_direct_send_reaches_each_return_class_with_its_arguments_intact() {
    let obj = StretProbe::alloc_init(mtm(), ()).expect("the probe instantiates");
    let this = obj.as_id();

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

#[test]
fn nsinvocation_drives_every_return_class_through_the_registered_encoding() {
    aterm_objc::autoreleasepool(|_| {
        let obj = StretProbe::alloc_init(mtm(), ()).expect("the probe instantiates");
        let this = obj.as_id();
        let cls = StretProbe::class();

        let s = sel!(firstRectForCharacterRange:actualRange:);
        let inv = invocation(this, cls, s);
        let range = NSRange {
            location: 101,
            length: 202,
        };
        let mut actual = NSRange::default();
        let mut actual_ptr: *mut NSRange = &raw mut actual;
        // SAFETY: indices 2 and 3 match the `NSRange` and `NSRange *` slots.
        unsafe {
            set_argument(inv, std::ptr::from_ref(&range).cast(), 2);
            set_argument(inv, std::ptr::from_mut(&mut actual_ptr).cast(), 3);
        }
        // SAFETY: `CGRect` is the return type the signature records.
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
