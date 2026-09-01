// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W2 second-pass adversarial arming: attacks on the W2 FIXES themselves.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_objc::{
    Bool, CGPoint, CGRect, CGSize, ClassPtr, ClassType, Encode, Id, Sel, autoreleasepool, class,
    class_of, declare_class, msg, sel,
};

/// 16 bytes — the last size that comes back in registers on x86_64.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct B16 {
    lo: u64,
    hi: u64,
}
// SAFETY: `#[repr(C)]` two `unsigned long long`, which is `{B16=QQ}`.
unsafe impl Encode for B16 {
    const ENCODING: &'static str = "{B16=QQ}";
}

/// 17 bytes — the first size that must go through `objc_msgSend_stret`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct B17(pub [u8; 17]);
// SAFETY: `#[repr(C)]` over a 17-byte char array.
unsafe impl Encode for B17 {
    const ENCODING: &'static str = "{B17=[17c]}";
}

struct Ivars {
    calls: Cell<i64>,
    drops: Arc<AtomicUsize>,
}
impl Drop for Ivars {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

declare_class! {
    /// Arities 4, 5 and 6 — past everything W1 or W2 exercised — plus the two
    /// struct sizes that straddle the `_stret` boundary.
    struct Adv: NSObject {
        const NAME: &str = "ATermObjcAdvW2";
        type Ivars = Ivars;

        @sel(a:b:c:d:)
        fn arity4(&self, a: i64, b: i64, c: i64, d: i64) -> i64 {
            a * 1000 + b * 100 + c * 10 + d
        }

        /// Five arguments of FIVE different encodings, so a register shift
        /// cannot cancel out.
        @sel(i:d:o:s:f:)
        fn arity5(&self, i: i64, d: f64, o: Id, s: Sel, f: f32) -> f64 {
            let o_bit = f64::from(u8::from(o.is_null()));
            let s_bit = f64::from(u8::from(s == sel!(insertNewline:)));
            (i as f64) * 100_000.0 + d * 1000.0 + o_bit * 100.0 + s_bit * 10.0 + f64::from(f)
        }

        /// Six — the width of the widest real selector in the tree
        /// (`addObserver:selector:name:object:` is four; winit's widest is six).
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

        /// D5, checked against the runtime's own retain count rather than
        /// against a drop counter.
        @sel(makeChild)
        fn make_child(&self) -> Id {
            match Adv::alloc_init(Ivars {
                calls: Cell::new(0),
                drops: Arc::clone(&self.ivars().drops),
            }) {
                Some(c) => c.autorelease(),
                None => std::ptr::null_mut(),
            }
        }

        /// The +1 form D5 says is a LEAK, kept so the test can show the
        /// difference rather than assert it.
        @sel(makeLeakedChild)
        fn make_leaked_child(&self) -> Id {
            match Adv::alloc_init(Ivars {
                calls: Cell::new(0),
                drops: Arc::clone(&self.ivars().drops),
            }) {
                Some(c) => c.into_raw(),
                None => std::ptr::null_mut(),
            }
        }

        @sel(blendRed:green:blue:)
        fn blend(&self, r: f64, g: f64, b: f64) -> f64 {
            r * 100.0 + g * 10.0 + b
        }

        @sel(isPositive:)
        fn is_positive(&self, n: i64) -> Bool {
            Bool::new(n > 0)
        }

        @sel(boom)
        fn boom(&self) {
            panic!("a declared method panicked");
        }

        @sel(count)
        fn count(&self) -> i64 {
            self.ivars().calls.get()
        }
    }
}

fn adv(drops: &Arc<AtomicUsize>) -> aterm_objc::Retained<Adv> {
    Adv::alloc_init(Ivars {
        calls: Cell::new(0),
        drops: Arc::clone(drops),
    })
    .expect("+alloc/-init")
}

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
}

fn encoding_of(cls: ClassPtr, name: &'static CStr) -> String {
    // SAFETY: side-effect-free runtime queries on a registered class; the
    // string is owned by the runtime.
    unsafe {
        let m = class_getInstanceMethod(cls, aterm_objc::sel_uncached(name));
        assert!(!m.is_null(), "no method {name:?}");
        CStr::from_ptr(method_getTypeEncoding(m))
            .to_string_lossy()
            .into_owned()
    }
}

// ---------------------------------------------------------------- arity 4..6

#[test]
fn arity_four_five_and_six_reach_their_bodies_with_arguments_in_order() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = adv(&drops);
    // SAFETY: each prototype is the exact C signature declared above, on a
    // live instance of the declaring class.
    unsafe {
        let f4: unsafe extern "C" fn(Id, Sel, i64, i64, i64, i64) -> i64 = msg();
        assert_eq!(f4(obj.as_id(), sel!(a:b:c:d:), 1, 2, 3, 4), 1234);
        assert_eq!(f4(obj.as_id(), sel!(a:b:c:d:), 9, 0, 0, 1), 9001);

        let f5: unsafe extern "C" fn(Id, Sel, i64, f64, Id, Sel, f32) -> f64 = msg();
        let got = f5(
            obj.as_id(),
            sel!(i:d:o:s:f:),
            7,
            0.5,
            std::ptr::null_mut(),
            sel!(insertNewline:),
            0.25,
        );
        assert!(
            (got - (700_000.0 + 500.0 + 100.0 + 10.0 + 0.25)).abs() < 1e-9,
            "arity-5 arguments arrived as {got}"
        );

        let f6: unsafe extern "C" fn(Id, Sel, i8, i16, i32, i64, Bool, CGPoint) -> f64 = msg();
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
    let cls = Adv::class();
    assert_eq!(encoding_of(cls, c"a:b:c:d:"), "q@:qqqq");
    assert_eq!(encoding_of(cls, c"i:d:o:s:f:"), "d@:qd@:f");
    assert_eq!(
        encoding_of(cls, c"p:q:r:s:t:u:"),
        format!("d@:csiq{}{{CGPoint=dd}}", Bool::ENCODING)
    );
    assert_eq!(encoding_of(cls, c"sixteen"), "{B16=QQ}@:");
    assert_eq!(encoding_of(cls, c"seventeen"), "{B17=[17c]}@:");
}

// ------------------------------------------------- 16 vs 17 stret straddle

#[test]
fn sixteen_and_seventeen_byte_returns_both_round_trip() {
    assert_eq!(size_of::<B16>(), 16);
    assert_eq!(size_of::<B17>(), 17);
    assert!(!aterm_objc::returns_indirectly::<B16>());
    assert_eq!(
        aterm_objc::returns_indirectly::<B17>(),
        cfg!(target_arch = "x86_64")
    );

    let drops = Arc::new(AtomicUsize::new(0));
    let obj = adv(&drops);
    // SAFETY: exact prototypes of `-sixteen`/`-seventeen`/`-echo…`, live
    // instance. On x86_64 the 17-byte pair goes through `objc_msgSend_stret`
    // and the 16-byte pair does not; `msg` picks, not this call site.
    unsafe {
        let g16: unsafe extern "C" fn(Id, Sel) -> B16 = msg();
        assert_eq!(
            g16(obj.as_id(), sel!(sixteen)),
            B16 {
                lo: 0x0102_0304_0506_0708,
                hi: 0x1112_1314_1516_1718
            }
        );
        let g17: unsafe extern "C" fn(Id, Sel) -> B17 = msg();
        assert_eq!(
            g17(obj.as_id(), sel!(seventeen)),
            B17([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17])
        );
        // Arguments AND return of each size, so a shifted register shows up.
        let e16: unsafe extern "C" fn(Id, Sel, B16) -> B16 = msg();
        assert_eq!(
            e16(obj.as_id(), sel!(echoSixteen:), B16 { lo: 11, hi: 22 }),
            B16 { lo: 22, hi: 11 }
        );
        let e17: unsafe extern "C" fn(Id, Sel, B17) -> B17 = msg();
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
fn a_thirty_two_byte_send_and_a_sixteen_byte_send_agree_with_the_c_abi() {
    // Entry-point identity, at the exact boundary rather than at 32 bytes.
    // SAFETY: no pointer here is called; only its address is read.
    let (plain, b16, b17) = unsafe {
        let a: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let b: unsafe extern "C" fn(Id, Sel) -> B16 = msg();
        let c: unsafe extern "C" fn(Id, Sel) -> B17 = msg();
        (a as usize, b as usize, c as usize)
    };
    assert_eq!(plain, b16, "a 16-byte return never needs _stret");
    if cfg!(target_arch = "x86_64") {
        assert_ne!(plain, b17, "17 bytes is the first size that needs _stret");
    } else {
        assert_eq!(plain, b17, "arm64 libobjc exports no _stret variant");
    }
    let _ = CGRect::default();
    let _ = CGSize::default();
}

// ------------------------------------------------ D5 by real retain count

#[test]
fn an_autoreleased_return_is_at_plus_one_owned_by_the_pool() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = adv(&drops);
    // SAFETY: `-retainCount` is `-(NSUInteger)`, `-makeChild` is `-(id)`, both
    // on live instances of the declaring class.
    unsafe {
        let rc: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        assert_eq!(rc(obj.as_id(), sel!(retainCount)), 1, "alloc/init is +1");

        autoreleasepool(|_| {
            let child = {
                let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
                f(obj.as_id(), sel!(makeChild))
            };
            assert!(!child.is_null());
            assert_eq!(class_of(child), Adv::class());
            assert_eq!(
                rc(child, sel!(retainCount)),
                1,
                "an autoreleased return must be at +1 held BY THE POOL — 2 means \
                 over-retained, and a freed object means under-retained"
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "the pool did not release the autoreleased return exactly once"
        );

        // The +1 form, for contrast: it survives the pool, which is the leak.
        let leaked = autoreleasepool(|_| {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            f(obj.as_id(), sel!(makeLeakedChild))
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "a `into_raw` return is NOT released by the pool — this is the leak \
             D5 names, kept here so the difference is measured"
        );
        // Clean it up so the test leaks nothing.
        let rel: unsafe extern "C" fn(Id, Sel) = msg();
        rel(leaked, sel!(release));
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn a_caller_may_retain_an_autoreleased_return_across_the_pool() {
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = adv(&drops);
    // SAFETY: exact prototypes; `child` is a live autoreleased instance for
    // the body of the pool and is retained before the pool pops.
    unsafe {
        let retained = autoreleasepool(|_| {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let child = f(obj.as_id(), sel!(makeChild));
            aterm_objc::Obj::retain(child).expect("live")
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "the retained object died with the pool — the return was under-retained"
        );
        let rc: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        assert_eq!(rc(retained.id(), sel!(retainCount)), 1);
        drop(retained);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

// ------------------------------- the encoding, read by Foundation itself

#[test]
fn nsinvocation_reads_our_arity_and_types_and_invokes_correctly() {
    // The reflective path D1 and D3 are ABOUT: Foundation builds an
    // `NSMethodSignature` from the encoding this crate registered, and reads
    // argument count, widths and the return type out of it.
    let drops = Arc::new(AtomicUsize::new(0));
    let obj = adv(&drops);
    let cls = Adv::class();
    // SAFETY: every prototype below is the documented signature of the
    // Foundation method being sent, on live objects.
    unsafe {
        let sig_for: unsafe extern "C" fn(ClassPtr, Sel, Sel) -> Id = msg();
        let sig = sig_for(
            cls,
            sel!(instanceMethodSignatureForSelector:),
            sel!(blendRed:green:blue:),
        );
        assert!(!sig.is_null(), "Foundation refused our type encoding");

        let n_args: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        assert_eq!(
            n_args(sig, sel!(numberOfArguments)),
            5,
            "self, _cmd and THREE arguments — this is the number AppKit reads \
             arity from, and the one D1 could not even express"
        );

        let arg_type: unsafe extern "C" fn(Id, Sel, usize) -> *const c_char = msg();
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
        let ret_type: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
        assert_eq!(CStr::from_ptr(ret_type(sig, sel!(methodReturnType))), c"d");
        let ret_len: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        assert_eq!(ret_len(sig, sel!(methodReturnLength)), 8);

        // Now actually INVOKE through NSInvocation.
        autoreleasepool(|_| {
            let inv_with: unsafe extern "C" fn(ClassPtr, Sel, Id) -> Id = msg();
            let inv = inv_with(
                class(c"NSInvocation"),
                sel!(invocationWithMethodSignature:),
                sig,
            );
            assert!(!inv.is_null());
            let set_target: unsafe extern "C" fn(Id, Sel, Id) = msg();
            set_target(inv, sel!(setTarget:), obj.as_id());
            let set_sel: unsafe extern "C" fn(Id, Sel, Sel) = msg();
            set_sel(inv, sel!(setSelector:), sel!(blendRed:green:blue:));
            let set_arg: unsafe extern "C" fn(Id, Sel, *const c_void, usize) = msg();
            let (r, g, b) = (1.0_f64, 2.0_f64, 3.0_f64);
            set_arg(inv, sel!(setArgument:atIndex:), (&raw const r).cast(), 2);
            set_arg(inv, sel!(setArgument:atIndex:), (&raw const g).cast(), 3);
            set_arg(inv, sel!(setArgument:atIndex:), (&raw const b).cast(), 4);
            let invoke: unsafe extern "C" fn(Id, Sel) = msg();
            invoke(inv, sel!(invoke));
            let mut out = 0.0_f64;
            let get_ret: unsafe extern "C" fn(Id, Sel, *mut c_void) = msg();
            get_ret(inv, sel!(getReturnValue:), (&raw mut out).cast());
            assert!(
                (out - 123.0).abs() < f64::EPSILON,
                "NSInvocation read our encoding and produced {out}, not 123"
            );
        });
    }
}

#[test]
fn nsmethodsignature_agrees_with_foundation_on_what_bool_is() {
    // D3 through the reflective path, arch-adaptive: the return type
    // Foundation reports for OUR `BOOL` method must be the same letter it
    // reports for its OWN.
    let cls = Adv::class();
    // SAFETY: documented Foundation signatures on live class objects.
    unsafe {
        let sig_for: unsafe extern "C" fn(ClassPtr, Sel, Sel) -> Id = msg();
        let ours = sig_for(
            cls,
            sel!(instanceMethodSignatureForSelector:),
            sel!(isPositive:),
        );
        let theirs = sig_for(
            class(c"NSObject"),
            sel!(instanceMethodSignatureForSelector:),
            sel!(isEqual:),
        );
        assert!(!ours.is_null() && !theirs.is_null());
        let ret_type: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
        let a = CStr::from_ptr(ret_type(ours, sel!(methodReturnType))).to_owned();
        let b = CStr::from_ptr(ret_type(theirs, sel!(methodReturnType))).to_owned();
        assert_eq!(
            a, b,
            "Foundation reads our BOOL return as {a:?} and its own as {b:?}"
        );
        assert_eq!(a.to_str().unwrap(), Bool::ENCODING);
    }
}

// ------------------------------------------------ blocks with a Drop capture

#[test]
fn a_block_capturing_a_drop_type_drops_it_exactly_once_when_the_block_dies() {
    let drops = Arc::new(AtomicUsize::new(0));
    struct Noisy(Arc<AtomicUsize>);
    impl Drop for Noisy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let ran = Arc::new(AtomicUsize::new(0));
    {
        let noisy = Noisy(Arc::clone(&drops));
        let ran2 = Arc::clone(&ran);
        // SAFETY: the prototype is `void (^)(void)`, which is what `new0`
        // builds, and the block is only invoked through that prototype below.
        let block = unsafe {
            aterm_objc::RcBlock::new0(move || {
                let _ = &noisy;
                ran2.fetch_add(1, Ordering::SeqCst);
            })
        }
        .expect("_Block_copy");
        assert_eq!(drops.load(Ordering::SeqCst), 0, "dropped at construction");

        // Invoke it the way a framework does: through the block's own `invoke`.
        // SAFETY: `block` is a live heap block built by `new0`, whose header
        // holds an `invoke` of exactly this prototype and whose first argument
        // is the block itself.
        unsafe {
            let p = block.as_ptr();
            let invoke = *p.cast::<*const c_void>().add(2); // isa,flags+reserved,invoke
            let f: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(invoke);
            f(p);
            f(p);
        }
        assert_eq!(ran.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "invoking must not consume");

        // A framework retain, then release: still exactly one drop at the end.
        let second = block.clone_retained();
        drop(block);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "released at +2");
        drop(second);
    }
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the capture was not dropped exactly once by the dispose helper"
    );
}

#[test]
fn a_block_that_is_never_invoked_still_drops_its_capture_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    struct Noisy(Arc<AtomicUsize>);
    impl Drop for Noisy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    {
        let noisy = Noisy(Arc::clone(&drops));
        // SAFETY: the block's prototype is `id (^)(id)`; it is never invoked.
        let _b = unsafe {
            aterm_objc::RcBlock::new1(move |x: Id| {
                let _ = &noisy;
                x
            })
        }
        .expect("_Block_copy");
    }
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn a_block_handed_to_a_real_framework_api_survives_and_is_released_once() {
    // `NSArray -enumerateObjectsUsingBlock:` copies and releases the block
    // itself, which is the path all three real sites take.
    let drops = Arc::new(AtomicUsize::new(0));
    let hits = Arc::new(AtomicUsize::new(0));
    struct Noisy(Arc<AtomicUsize>);
    impl Drop for Noisy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    autoreleasepool(|_| {
        let noisy = Noisy(Arc::clone(&drops));
        let hits2 = Arc::clone(&hits);
        // SAFETY: the enumeration block is
        // `void (^)(id obj, NSUInteger idx, BOOL *stop)`; `new2` builds an
        // arity-2 prototype, so only the first two arguments are declared and
        // the third is ignored — legal on both Apple ABIs, where surplus
        // arguments sit in registers the callee never reads.
        let block = unsafe {
            aterm_objc::RcBlock::new2(move |_obj: Id, _idx: usize| {
                let _ = &noisy;
                hits2.fetch_add(1, Ordering::SeqCst);
            })
        }
        .expect("_Block_copy");

        // SAFETY: `+arrayWithObjects:count:` and
        // `-enumerateObjectsUsingBlock:` are exactly these prototypes.
        unsafe {
            let s = aterm_objc::ns_string("x").expect("NSString");
            let objs = [s.id(), s.id(), s.id()];
            let arr_with: unsafe extern "C" fn(ClassPtr, Sel, *const Id, usize) -> Id = msg();
            let arr = arr_with(
                class(c"NSArray"),
                sel!(arrayWithObjects:count:),
                objs.as_ptr(),
                3,
            );
            assert!(!arr.is_null());
            let enumerate: unsafe extern "C" fn(Id, Sel, *mut c_void) = msg();
            enumerate(arr, sel!(enumerateObjectsUsingBlock:), block.as_ptr());
        }
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "dropped while in use");
        drop(block);
    });
    assert_eq!(
        drops.load(Ordering::SeqCst),
        1,
        "the capture was dropped {} times after a real framework round trip",
        drops.load(Ordering::SeqCst)
    );
}

// ------------------------------------------------ sel cache, concurrent

#[test]
fn the_sel_cache_is_correct_under_a_hard_concurrent_first_use() {
    // Many threads, many DISTINCT slots, every slot's first use racing. The
    // claim under test is that the relaxed store is benign BY VALUE: every
    // thread must observe the same interned pointer, and it must equal the
    // uncached lookup.
    const THREADS: usize = 16;
    let barrier = Arc::new(std::sync::Barrier::new(THREADS));
    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let b = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            b.wait();
            let mut v = Vec::new();
            for _ in 0..64 {
                v.push(sel!(objectAtIndex:) as usize);
                v.push(sel!(initWithBytes:length:encoding:) as usize);
                v.push(sel!(description) as usize);
                v.push(sel!(isEqualToString:) as usize);
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
        aterm_objc::sel_uncached(c"objectAtIndex:") as usize
    );
    assert_eq!(
        first[1],
        aterm_objc::sel_uncached(c"initWithBytes:length:encoding:") as usize
    );
}

// ------------------------------------- a panic inside framework dispatch

/// Set in the child so it performs the panicking send instead of re-spawning.
const PANIC_CHILD: &str = "ATERM_OBJC_W2_PANIC_CHILD";
const SIGABRT: i32 = 6;

#[test]
fn a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation() {
    use std::os::unix::process::ExitStatusExt as _;
    if std::env::var_os(PANIC_CHILD).is_some() {
        let drops = Arc::new(AtomicUsize::new(0));
        let obj = adv(&drops);
        // Dispatched by FOUNDATION, not by a direct call: `-performSelector:`
        // puts a framework frame between the send and our trampoline, which is
        // the shape `NSNotificationCenter` and AppKit's responder chain use.
        // SAFETY: `-performSelector:` is `-(id)performSelector:(SEL)`, and the
        // target is a live instance that declares `-boom`.
        unsafe {
            let perform: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
            perform(obj.as_id(), sel!(performSelector:), sel!(boom));
        }
        println!("SURVIVED-THE-PANIC");
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "--nocapture",
            "a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation",
        ])
        .env(PANIC_CHILD, "1")
        .output()
        .expect("re-running this binary as a child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("SURVIVED-THE-PANIC"),
        "the panic did not abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "expected SIGABRT\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("panic escaped Objective-C method `boom`"),
        "aborted, but not through aterm-objc's own guard:\n{stderr}"
    );
}
