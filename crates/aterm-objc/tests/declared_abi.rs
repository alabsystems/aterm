// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The C ABI of DECLARED methods and of the sends that reach them.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "the subject is the send itself: these tests cast the bare `msg` by hand"
)]

use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;

use aterm_objc::{
    BlockPtr, Bool, CGPoint, ClassPtr, ClassType, Encode, Id, ProtocolPtr, Sel, autoreleasepool,
    class, declare_class, msg, protocol, sel, sel_uncached, strip_method_offsets,
};

fn mtm() -> aterm_objc::MainThread {
    // SAFETY: test classes here have no main-thread affinity and live on this worker.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
    fn method_copyArgumentType(m: *const c_void, index: u32) -> *mut c_char;
    fn free(p: *mut c_void);
}

fn encoding_of(cls: ClassPtr, name: &'static CStr) -> String {
    // SAFETY: side-effect-free runtime queries on a registered class; the runtime owns the
    // returned string and it outlives the read.
    unsafe {
        let m = class_getInstanceMethod(cls, sel_uncached(name));
        assert!(!m.is_null(), "{name:?} is not a method of this class");
        CStr::from_ptr(method_getTypeEncoding(m))
            .to_string_lossy()
            .into_owned()
    }
}

/// 16 bytes — the last size that comes back in registers on x86_64.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct B16 {
    lo: u64,
    hi: u64,
}
unsafe impl Encode for B16 {
    const ENCODING: &'static str = "{B16=QQ}";
}

unsafe impl aterm_objc::InertZero for B16 {}

/// 17 bytes — the first size that must go through `objc_msgSend_stret`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct B17(pub [u8; 17]);
unsafe impl Encode for B17 {
    const ENCODING: &'static str = "{B17=[17c]}";
}
unsafe impl aterm_objc::InertZero for B17 {}

declare_class! {
    struct Arity: NSObject {
        const NAME: &str = "ATermObjcArityProbe";
        type Ivars = ();

        @sel(a:b:c:d:)
        fn arity4(&self, a: i64, b: i64, c: i64, d: i64) -> i64 {
            a * 1000 + b * 100 + c * 10 + d
        }

        @sel(i:d:o:s:f:)
        fn arity5(&self, i: i64, d: f64, o: Id, s: Sel, f: f32) -> f64 {
            let o_bit = f64::from(u8::from(o.is_null()));
            let s_bit = f64::from(u8::from(s == sel!(insertNewline:)));
            (i as f64) * 100_000.0 + d * 1000.0 + o_bit * 100.0 + s_bit * 10.0 + f64::from(f)
        }

        @sel(p:q:r:s:t:u:)
        fn arity6(&self, p: i8, q: i16, r: i32, s: i64, t: Bool, u: CGPoint) -> f64 {
            f64::from(p) + f64::from(q) + f64::from(r) + (s as f64)
                + f64::from(u8::from(t.as_bool())) + u.x + u.y
        }

        @sel(sixteen)
        fn sixteen(&self) -> B16 {
            B16 { lo: 0x0102_0304_0506_0708, hi: 0x1112_1314_1516_1718 }
        }

        @sel(seventeen)
        fn seventeen(&self) -> B17 {
            B17([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17])
        }

        @sel(echoSixteen:)
        fn echo16(&self, v: B16) -> B16 {
            B16 { lo: v.hi, hi: v.lo }
        }

        @sel(echoSeventeen:)
        fn echo17(&self, v: B17) -> B17 {
            let mut out = v;
            out.0.reverse();
            out
        }

        @sel(blendRed:green:blue:)
        fn blend(&self, r: f64, g: f64, b: f64) -> f64 {
            r * 100.0 + g * 10.0 + b
        }

        @sel(isPositive:)
        fn is_positive(&self, n: i64) -> Bool {
            Bool::new(n > 0)
        }
    }
}

fn arity() -> aterm_objc::Retained<Arity> {
    Arity::alloc_init(mtm(), ()).expect("+alloc/-init")
}

#[test]
fn arity_four_five_and_six_reach_their_bodies_with_arguments_in_order() {
    let obj = arity();
    // SAFETY: each prototype is the exact C signature declared above, on a live instance of the
    // declaring class.
    unsafe {
        let f4: unsafe extern "C-unwind" fn(Id, Sel, i64, i64, i64, i64) -> i64 = msg();
        assert_eq!(f4(obj.as_id(), sel!(a:b:c:d:), 1, 2, 3, 4), 1234);
        assert_eq!(f4(obj.as_id(), sel!(a:b:c:d:), 9, 0, 0, 1), 9001);

        let f5: unsafe extern "C-unwind" fn(Id, Sel, i64, f64, Id, Sel, f32) -> f64 = msg();
        let got = f5(
            obj.as_id(),
            sel!(i:d:o:s:f:),
            7,
            0.5,
            Id::NIL,
            sel!(insertNewline:),
            0.25,
        );
        assert!(
            (got - (700_000.0 + 500.0 + 100.0 + 10.0 + 0.25)).abs() < 1e-9,
            "arity-5 arguments arrived as {got}"
        );

        let f6: unsafe extern "C-unwind" fn(Id, Sel, i8, i16, i32, i64, Bool, CGPoint) -> f64 =
            msg();
        let got6 = f6(
            obj.as_id(),
            sel!(p:q:r:s:t:u:),
            1,
            2,
            3,
            4,
            Bool::YES,
            CGPoint { x: 0.5, y: 0.25 },
        );
        assert!(
            (got6 - (1.0 + 2.0 + 3.0 + 4.0 + 1.0 + 0.5 + 0.25)).abs() < 1e-9,
            "arity-6 arguments arrived as {got6}"
        );
    }
}

#[test]
fn the_runtime_reports_the_arity_four_five_and_six_encodings() {
    let cls = Arity::class();
    assert_eq!(encoding_of(cls, c"a:b:c:d:"), "q@:qqqq");
    assert_eq!(encoding_of(cls, c"i:d:o:s:f:"), "d@:qd@:f");
    assert_eq!(
        encoding_of(cls, c"p:q:r:s:t:u:"),
        format!("d@:csiq{}{{CGPoint=dd}}", Bool::ENCODING)
    );
    assert_eq!(encoding_of(cls, c"sixteen"), "{B16=QQ}@:");
    assert_eq!(encoding_of(cls, c"seventeen"), "{B17=[17c]}@:");
}

#[test]
fn sixteen_and_seventeen_byte_returns_both_round_trip() {
    assert_eq!(size_of::<B16>(), 16);
    assert_eq!(size_of::<B17>(), 17);
    assert!(!aterm_objc::returns_indirectly::<B16>());
    assert_eq!(
        aterm_objc::returns_indirectly::<B17>(),
        cfg!(target_arch = "x86_64")
    );

    let obj = arity();
    // SAFETY: exact prototypes of `-sixteen`/`-seventeen`/`-echo…`, live instance.
    unsafe {
        let g16: unsafe extern "C-unwind" fn(Id, Sel) -> B16 = msg();
        assert_eq!(
            g16(obj.as_id(), sel!(sixteen)),
            B16 {
                lo: 0x0102_0304_0506_0708,
                hi: 0x1112_1314_1516_1718
            }
        );
        let g17: unsafe extern "C-unwind" fn(Id, Sel) -> B17 = msg();
        assert_eq!(
            g17(obj.as_id(), sel!(seventeen)),
            B17([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17])
        );
        let e16: unsafe extern "C-unwind" fn(Id, Sel, B16) -> B16 = msg();
        assert_eq!(
            e16(obj.as_id(), sel!(echoSixteen:), B16 { lo: 11, hi: 22 }),
            B16 { lo: 22, hi: 11 }
        );
        let e17: unsafe extern "C-unwind" fn(Id, Sel, B17) -> B17 = msg();
        let mut expect = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17];
        expect.reverse();
        assert_eq!(
            e17(
                obj.as_id(),
                sel!(echoSeventeen:),
                B17([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17])
            ),
            B17(expect)
        );
    }
}

#[test]
fn nsinvocation_reads_our_arity_and_types_and_invokes_correctly() {
    let obj = arity();
    let cls = Arity::class();
    // SAFETY: every prototype below matches the documented Foundation method signature, on live
    // objects.
    unsafe {
        let sig_for: unsafe extern "C-unwind" fn(ClassPtr, Sel, Sel) -> Id = msg();
        let sig = sig_for(
            cls,
            sel!(instanceMethodSignatureForSelector:),
            sel!(blendRed:green:blue:),
        );
        assert!(!sig.is_null(), "Foundation refused our type encoding");

        let n_args: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
        assert_eq!(
            n_args(sig, sel!(numberOfArguments)),
            5,
            "self, _cmd and THREE arguments — this is the number AppKit reads \
             arity from, and the one D1 could not even express"
        );

        let arg_type: unsafe extern "C-unwind" fn(Id, Sel, usize) -> *const c_char = msg();
        assert_eq!(
            CStr::from_ptr(arg_type(sig, sel!(getArgumentTypeAtIndex:), 0)),
            c"@"
        );
        assert_eq!(
            CStr::from_ptr(arg_type(sig, sel!(getArgumentTypeAtIndex:), 1)),
            c":"
        );
        for i in 2..5 {
            assert_eq!(
                CStr::from_ptr(arg_type(sig, sel!(getArgumentTypeAtIndex:), i)),
                c"d",
                "argument {i} is not a double"
            );
        }
        let ret_type: unsafe extern "C-unwind" fn(Id, Sel) -> *const c_char = msg();
        assert_eq!(CStr::from_ptr(ret_type(sig, sel!(methodReturnType))), c"d");
        let ret_len: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
        assert_eq!(ret_len(sig, sel!(methodReturnLength)), 8);

        autoreleasepool(|_| {
            let inv_with: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id) -> Id = msg();
            let inv = inv_with(
                class(c"NSInvocation"),
                sel!(invocationWithMethodSignature:),
                sig,
            );
            assert!(!inv.is_null());
            let set_target: unsafe extern "C-unwind" fn(Id, Sel, Id) = msg();
            set_target(inv, sel!(setTarget:), obj.as_id());
            let set_sel: unsafe extern "C-unwind" fn(Id, Sel, Sel) = msg();
            set_sel(inv, sel!(setSelector:), sel!(blendRed:green:blue:));
            let set_arg: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize) = msg();
            let (r, g, b) = (1.0_f64, 2.0_f64, 3.0_f64);
            set_arg(inv, sel!(setArgument:atIndex:), (&raw const r).cast(), 2);
            set_arg(inv, sel!(setArgument:atIndex:), (&raw const g).cast(), 3);
            set_arg(inv, sel!(setArgument:atIndex:), (&raw const b).cast(), 4);
            let invoke: unsafe extern "C-unwind" fn(Id, Sel) = msg();
            invoke(inv, sel!(invoke));
            let mut out = 0.0_f64;
            let get_ret: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) = msg();
            get_ret(inv, sel!(getReturnValue:), (&raw mut out).cast());
            assert!(
                (out - 123.0).abs() < f64::EPSILON,
                "NSInvocation read our encoding and produced {out}, not 123"
            );
        });
    }
}

declare_class! {
    struct Empty: NSObject {
        const NAME: &str = "ATermObjcEmptyProbe";
        type Ivars = ();
    }
}

#[test]
fn concurrent_first_registration_produces_one_class() {
    const N: usize = 8;
    let barrier = Arc::new(std::sync::Barrier::new(N));
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                b.wait();
                Empty::class().addr()
            })
        })
        .collect();
    let addrs: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(addrs.iter().all(|a| *a == addrs[0]), "{addrs:?}");
    assert_eq!(addrs[0], class(c"ATermObjcEmptyProbe").addr());
}

declare_class! {
    struct Widest: NSObject {
        const NAME: &str = "ATermObjcFourteenColonProbe";
        type Ivars = ();

        @sel(a:b:c:d:e:f:g:h:i:j:k:l:m:n:)
        fn widest(
            &self,
            a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64,
            h: i64, i: i64, j: i64, k: i64, l: i64, m: i64, n: i64,
        ) -> i64 {
            a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7
                + h * 8 + i * 9 + j * 10 + k * 11 + l * 12 + m * 13 + n * 14
        }
    }
}

#[test]
fn a_fourteen_argument_method_is_sendable_from_rust_not_just_declarable() {
    let obj = Widest::alloc_init(mtm(), ()).expect("+alloc/-init");
    // SAFETY: the exact declared 16-parameter prototype on a live instance of the class that
    // declares it.
    unsafe {
        #[rustfmt::skip]
        let f: unsafe extern "C-unwind" fn(
            Id, Sel,
            i64, i64, i64, i64, i64, i64, i64,
            i64, i64, i64, i64, i64, i64, i64,
        ) -> i64 = msg();
        let s = sel!(a:b:c:d:e:f:g:h:i:j:k:l:m:n:);
        assert_eq!(
            f(obj.as_id(), s, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1),
            105
        );
        assert_eq!(
            f(obj.as_id(), s, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
            1
        );
        assert_eq!(
            f(obj.as_id(), s, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1),
            14
        );
    }
    assert_eq!(
        encoding_of(Widest::class(), c"a:b:c:d:e:f:g:h:i:j:k:l:m:n:"),
        "q@:qqqqqqqqqqqqqq"
    );
}

declare_class! {
    struct Shapes: NSObject {
        const NAME: &str = "ATermObjcPointerShapesProbe";
        type Ivars = ();

        @sel(takeObject:)
        fn take_object(&self, o: Id) -> Bool { Bool::new(o.is_null()) }

        @sel(setDelegateClass:)
        fn set_delegate_class(&self, c: ClassPtr) -> Bool { Bool::new(c.is_null()) }

        @sel(takeProtocol:)
        fn take_protocol(&self, p: ProtocolPtr) -> Bool { Bool::new(p.is_null()) }

        @sel(takeSelector:)
        fn take_selector(&self, s: Sel) -> Bool { Bool::new(s.is_null()) }

        @sel(takeContext:)
        fn take_context(&self, p: *mut c_void) -> Bool { Bool::new(p.is_null()) }

        @sel(takeConstContext:)
        fn take_const_context(&self, p: *const c_void) -> Bool { Bool::new(p.is_null()) }

        @sel(takeBlock:)
        fn take_block(&self, _b: BlockPtr) {}
    }
}

#[test]
fn each_pointer_shaped_runtime_type_carries_its_own_encoding() {
    assert_eq!(<Id as Encode>::ENCODING, "@");
    assert_eq!(<ClassPtr as Encode>::ENCODING, "#");
    assert_eq!(<ProtocolPtr as Encode>::ENCODING, "@");
    assert_eq!(<Sel as Encode>::ENCODING, ":");
    assert_eq!(<*mut c_void as Encode>::ENCODING, "^v");
    assert_eq!(<*const c_void as Encode>::ENCODING, "^v");

    let cls = Shapes::class();
    let b = Bool::ENCODING;
    assert_eq!(encoding_of(cls, c"takeObject:"), format!("{b}@:@"));
    assert_eq!(
        encoding_of(cls, c"setDelegateClass:"),
        format!("{b}@:#"),
        "a Class argument encoded as `@` for a whole pass; clang emits \
         `v24@0:8#16` for `- (void)setDelegateClass:(Class)c`"
    );
    assert_eq!(encoding_of(cls, c"takeProtocol:"), format!("{b}@:@"));
    assert_eq!(encoding_of(cls, c"takeSelector:"), format!("{b}@::"));
    assert_eq!(
        encoding_of(cls, c"takeContext:"),
        format!("{b}@:^v"),
        "an opaque `void *` encoded as `@` — an OBJECT — which is what the KVO \
         `context:` argument every live site spells `*mut c_void` would have \
         been registered as"
    );
    assert_eq!(
        encoding_of(cls, c"takeConstContext:"),
        format!("{b}@:^v"),
        "`*const c_void` once encoded as `:`, a SELECTOR"
    );

    // SAFETY: `NSString` is a live, immortal Foundation class.
    let foundation = unsafe {
        aterm_objc::method_types(
            aterm_objc::class(c"NSString"),
            sel!(enumerateLinesUsingBlock:),
        )
    }
    .expect("Foundation implements enumerateLinesUsingBlock:");
    assert_eq!(strip_method_offsets(&foundation), "v@:@?");
    assert_eq!(encoding_of(cls, c"takeBlock:"), "v@:@?");

    let obj = Shapes::alloc_init(mtm(), ()).expect("+alloc/-init");
    // SAFETY: each prototype below is exactly the declared method's signature, on a live instance
    // of the class that declares it.
    unsafe {
        let f_cls: unsafe extern "C-unwind" fn(Id, Sel, ClassPtr) -> Bool = msg();
        assert!(!f_cls(obj.as_id(), sel!(setDelegateClass:), Shapes::class()).as_bool());
        assert!(f_cls(obj.as_id(), sel!(setDelegateClass:), ClassPtr::NULL).as_bool());

        let f_proto: unsafe extern "C-unwind" fn(Id, Sel, ProtocolPtr) -> Bool = msg();
        assert!(!f_proto(obj.as_id(), sel!(takeProtocol:), protocol(c"NSObject")).as_bool());

        let f_ctx: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Bool = msg();
        assert!(f_ctx(obj.as_id(), sel!(takeContext:), std::ptr::null_mut()).as_bool());
    }
}

#[test]
fn bool_ptr_encodes_what_foundation_compiled_for_its_bool_out_parameter() {
    let cls = aterm_objc::class(c"NSFileManager");
    assert!(!cls.is_null(), "Foundation's NSFileManager is registered");
    let name = c"fileExistsAtPath:isDirectory:";
    // SAFETY: side-effect-free runtime queries on a registered class.
    // `method_copyArgumentType` answers a `malloc`ed copy (null past the last argument), which
    // is read and then freed exactly once.
    let arg3 = unsafe {
        let m = class_getInstanceMethod(cls, sel_uncached(name));
        assert!(!m.is_null(), "{name:?} is a method of NSFileManager");
        let p = method_copyArgumentType(m, 3);
        assert!(!p.is_null(), "{name:?} has an argument 3");
        let s = CStr::from_ptr(p).to_string_lossy().into_owned();
        free(p.cast());
        s
    };
    let whole = encoding_of(cls, name);
    eprintln!("-[NSFileManager fileExistsAtPath:isDirectory:] = {whole}; argument 3 = {arg3}");
    assert_eq!(
        <*mut Bool as Encode>::ENCODING,
        arg3,
        "`*mut Bool` must encode what Foundation's own `BOOL *` argument does \
         (-[NSFileManager fileExistsAtPath:isDirectory:] is {whole})"
    );
}
