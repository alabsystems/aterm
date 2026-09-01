// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// W2 THIRD pass: the seven findings, each armed with the judge's own
// counterexample AT THE SPELLING THE LIVE SITES USE.
//
// | finding | the counterexample this file runs                                     |
// |---------|-----------------------------------------------------------------------|
// | P3-1    | KVO's `context:` at the `*mut c_void` spelling — see `adversary_w2c`   |
// | P3-2    | an over-aligned ivar; the 16-byte ceiling measured over 4,096 instances |
// | P3-3    | the WRAPPER around a packed struct — see `abi.rs`                      |
// | P3-4    | `setDelegateClass:(Class)`, whose encoding is `#` and never was        |
// | P3-6    | the `Encode` bound on every block argument and return                  |
// | P3-7    | `firstRectForCharacterRange:actualRange:` — `NSRange` by value AND ptr |
// | MsgFn   | a 15-argument method is now refused at the DECLARE end too             |
// | abort   | `abort_on_unwind` does not panic when stderr is unwritable             |

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};
use std::os::unix::process::ExitStatusExt as _;
use std::process::Command;

use aterm_objc::{
    Bool, CGRect, ClassPtr, ClassType, Encode, IVAR_NAME, Id, NSRange, ProtocolPtr, Sel,
    declare_class, method_encoding, msg, protocol, sel, sel_uncached,
};

/// `SIGABRT`, spelled out: this crate has ZERO dependencies by construction.
const SIGABRT: i32 = 6;

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
    fn class_getInstanceVariable(cls: ClassPtr, name: *const c_char) -> *const c_void;
    fn ivar_getOffset(ivar: *const c_void) -> isize;
}

/// The type encoding the runtime holds for `cls`'s `name`.
fn encoding_of(cls: ClassPtr, name: &'static CStr) -> String {
    // SAFETY: side-effect-free runtime queries on a registered class; the
    // string the runtime returns is owned by it and outlives the read.
    unsafe {
        let m = class_getInstanceMethod(cls, sel_uncached(name));
        assert!(!m.is_null(), "{name:?} is not a method of this class");
        CStr::from_ptr(method_getTypeEncoding(m))
            .to_string_lossy()
            .into_owned()
    }
}

// ---------------------------------------------------------------------------
// P3-1 / P3-4 — four pointer-shaped runtime types, four different letters.
//
// `Id`, `ClassPtr`, `ProtocolPtr` and `Sel` were all aliases of a raw void
// pointer, so the ONE `Encode` impl that existed for `*mut c_void` said `"@"`
// for all three `*mut` ones. `Sel` was newtyped in the second pass, which fixed
// `const void *` and left `*mut c_void` — the spelling the SDK, objc2 and
// vendored winit all use for KVO's `context:` — still registering as an OBJECT,
// and left `Class` registering as `@` where clang emits `#`.
// ---------------------------------------------------------------------------

declare_class! {
    /// One method per pointer-shaped type, so the four letters are read out of
    /// the runtime side by side.
    struct Shapes: NSObject {
        const NAME: &str = "ATermObjcShapesW2D";
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
    }
}

#[test]
fn each_pointer_shaped_runtime_type_carries_its_own_encoding() {
    // Static: the letters. MEASURED with clang on this box —
    //   @encode(id) = @   @encode(Class) = #   @encode(Protocol *) = @
    //   @encode(SEL) = :  @encode(void *) = ^v
    assert_eq!(<Id as Encode>::ENCODING, "@");
    assert_eq!(<ClassPtr as Encode>::ENCODING, "#");
    assert_eq!(<ProtocolPtr as Encode>::ENCODING, "@");
    assert_eq!(<Sel as Encode>::ENCODING, ":");
    assert_eq!(<*mut c_void as Encode>::ENCODING, "^v");

    // And they are four DIFFERENT Rust types, which is what makes four impls
    // possible at all. `Id` used to BE `*mut c_void`, so this line would not
    // have compiled — the two impls would have collided.
    assert_eq!(size_of::<Id>(), size_of::<*mut c_void>());
    assert_eq!(size_of::<ClassPtr>(), size_of::<*mut c_void>());
    assert_eq!(align_of::<Id>(), align_of::<*mut c_void>());

    // Measured: what the RUNTIME holds, which is what `NSMethodSignature`,
    // `NSInvocation`, forwarding and accessibility read.
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

    // The dynamic half: send each one, so the prototypes are exercised and not
    // merely written.
    let obj = Shapes::alloc_init(()).expect("+alloc/-init");
    // SAFETY: each prototype below is exactly the declared method's signature,
    // on a live instance of the class that declares it.
    unsafe {
        let f_cls: unsafe extern "C" fn(Id, Sel, ClassPtr) -> Bool = msg();
        assert!(!f_cls(obj.as_id(), sel!(setDelegateClass:), Shapes::class()).as_bool());
        assert!(f_cls(obj.as_id(), sel!(setDelegateClass:), ClassPtr::NULL).as_bool());

        let f_proto: unsafe extern "C" fn(Id, Sel, ProtocolPtr) -> Bool = msg();
        assert!(!f_proto(obj.as_id(), sel!(takeProtocol:), protocol(c"NSObject")).as_bool());

        let f_ctx: unsafe extern "C" fn(Id, Sel, *mut c_void) -> Bool = msg();
        assert!(f_ctx(obj.as_id(), sel!(takeContext:), std::ptr::null_mut()).as_bool());
    }
}

// ---------------------------------------------------------------------------
// P3-2 — an over-aligned ivar is UB reachable from safe code. `S4`'s twin.
// ---------------------------------------------------------------------------

/// Exactly at the ceiling: 16 bytes, which is what `class_createInstance`'s
/// `malloc` guarantees and therefore what an ivar may ask for.
#[repr(align(16))]
#[derive(Debug, PartialEq, Eq)]
struct AtTheCeiling(u64, u64);

declare_class! {
    /// The largest alignment an ivar may have. `align(32)` and `align(64)` are
    /// compile errors — see the `compile_fail` doctest on
    /// [`aterm_objc::ClassBuilder::add_rust_ivar`], which is where the refusal
    /// itself is armed, since a test that does not compile cannot live here.
    struct Aligned16: NSObject {
        const NAME: &str = "ATermObjcAligned16W2D";
        type Ivars = AtTheCeiling;

        @sel(sum)
        fn sum(&self) -> i64 {
            let v = self.ivars();
            i64::try_from(v.0 + v.1).expect("fits")
        }
    }
}

#[test]
fn an_ivar_at_the_sixteen_byte_ceiling_is_aligned_in_every_instance() {
    // This is the claim the ceiling rests on, and it is the one the crate got
    // wrong: `class_addIvar` aligns the ivar's OFFSET within the instance, but
    // the instance BASE is whatever `malloc` returns, and `malloc` promises 16
    // and no more. So the slot's real alignment is `min(16, requested)`.
    //
    // MEASURED here through `ivar_getOffset` and the raw `+alloc` addresses —
    // no Rust reference anywhere in the loop, so nothing is folded away.
    // Recorded at the time this test was written, 4,096 instances per class:
    //
    //   Ivars align  ivar offset  slot misaligned   base not 32-aligned
    //          16         16         0 / 4096          2042 / 4096
    //          32         32        19 / 4096            19 / 4096
    //          64         64         9 / 4096             9 / 4096
    //
    // The last two rows no longer compile. The first is asserted below, and the
    // `base not 32-aligned` column is why: the base really is only 16-aligned,
    // not accidentally more.
    let cls = Aligned16::class();
    // SAFETY: `cls` is registered, so its ivar table is final and the handle is
    // live for the process.
    let off = unsafe {
        let iv = class_getInstanceVariable(cls, IVAR_NAME.as_ptr());
        assert!(!iv.is_null(), "the class registered without its ivar");
        ivar_getOffset(iv)
    };
    assert!(
        usize::try_from(off)
            .expect("a non-negative ivar offset")
            .is_multiple_of(16),
        "class_addIvar aligned the OFFSET"
    );

    let mut misaligned = 0_u32;
    let mut base_not_32 = 0_u32;
    // SAFETY: `+alloc` on a registered class returns a +1, zero-filled instance
    // whose address is only read, never dereferenced. The instances are
    // deliberately leaked: `-init` never runs, so releasing them is not this
    // test's business, and 4,096 of them is a few hundred kilobytes.
    unsafe {
        let alloc: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
        for _ in 0..4096 {
            let base = alloc(cls, sel!(alloc)).expose_provenance();
            if !base.is_multiple_of(32) {
                base_not_32 += 1;
            }
            if !base.wrapping_add_signed(off).is_multiple_of(16) {
                misaligned += 1;
            }
        }
    }
    assert_eq!(
        misaligned, 0,
        "a 16-aligned ivar must land aligned in EVERY instance"
    );
    assert!(
        base_not_32 > 0,
        "if every instance base were 32-aligned this measurement would prove \
         nothing about the 16-byte ceiling; {base_not_32}/4096 were not"
    );

    // And the class works end to end through the SAFE `ivars()` accessor, which
    // is the caller that could not discharge `IvarSlot::get`'s alignment
    // precondition and now can, because no over-aligned class exists.
    let obj = Aligned16::alloc_init(AtTheCeiling(20, 22)).expect("+alloc/-init");
    // SAFETY: `-sum` is declared on this class with exactly this prototype.
    let sum = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> i64 = msg();
        f(obj.as_id(), sel!(sum))
    };
    assert_eq!(sum, 42);
    assert_eq!(obj.ivars(), &AtTheCeiling(20, 22));
}

// ---------------------------------------------------------------------------
// P3-7 — `NSRange`, and a pointer to it. Without both, winit's
// `NSTextInputClient` conformance cannot be DECLARED.
// ---------------------------------------------------------------------------

declare_class! {
    /// Three of the six `NSTextInputClient` methods that mention `NSRange`:
    /// one takes it by value AND by pointer, one returns it, one takes it.
    struct TextInput: NSObject {
        const NAME: &str = "ATermObjcTextInputW2D";
        type Ivars = ();

        @sel(firstRectForCharacterRange:actualRange:)
        fn first_rect(&self, range: NSRange, actual: *mut NSRange) -> CGRect {
            if !actual.is_null() {
                // SAFETY: `actualRange:` is an OUT parameter; the framework
                // passes either NULL or a writable `NSRange` it owns for the
                // duration of the call. Here the caller is the test below.
                unsafe { actual.write(range) };
            }
            CGRect {
                origin: aterm_objc::CGPoint { x: 1.0, y: 2.0 },
                size: aterm_objc::CGSize {
                    width: 3.0,
                    height: 4.0,
                },
            }
        }

        @sel(selectedRange)
        fn selected_range(&self) -> NSRange {
            NSRange {
                location: 7,
                length: 11,
            }
        }

        @sel(setMarkedRange:)
        fn set_marked_range(&self, r: NSRange) -> Bool {
            Bool::new(r.length == 0)
        }
    }
}

#[test]
fn an_nsrange_crosses_the_boundary_by_value_and_by_pointer() {
    // Static. MEASURED with clang on this box, from a compiled
    // `@implementation` of the same three declarations:
    //
    //   firstRectForCharacterRange:actualRange:
    //     {CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32
    //
    // The struct TAG is `_NSRange` (`NSRange` is a typedef) and `Q` is
    // `unsigned long long`; a pointer to a struct is `^` then the pointee's own
    // encoding.
    assert_eq!(<NSRange as Encode>::ENCODING, "{_NSRange=QQ}");
    assert_eq!(<*mut NSRange as Encode>::ENCODING, "^{_NSRange=QQ}");
    assert_eq!(
        method_encoding!(CGRect ; NSRange, *mut NSRange),
        "{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}"
    );

    let cls = TextInput::class();
    assert_eq!(
        encoding_of(cls, c"firstRectForCharacterRange:actualRange:"),
        "{CGRect={CGPoint=dd}{CGSize=dd}}@:{_NSRange=QQ}^{_NSRange=QQ}"
    );
    assert_eq!(encoding_of(cls, c"selectedRange"), "{_NSRange=QQ}@:");
    assert_eq!(
        encoding_of(cls, c"setMarkedRange:"),
        format!("{}@:{{_NSRange=QQ}}", Bool::ENCODING)
    );

    // Dynamic: the layout is right, not just the string. An `NSRange` is two
    // `NSUInteger`s in declaration order, which is what a wrong layout would
    // scramble.
    assert_eq!(size_of::<NSRange>(), 16);
    assert_eq!(align_of::<NSRange>(), 8);

    let obj = TextInput::alloc_init(()).expect("+alloc/-init");
    let mut actual = NSRange::default();
    // SAFETY: the three prototypes are exactly the declared signatures. The
    // out-pointer addresses a live local for the duration of the send.
    unsafe {
        let rect: unsafe extern "C" fn(Id, Sel, NSRange, *mut NSRange) -> CGRect = msg();
        let r = rect(
            obj.as_id(),
            sel!(firstRectForCharacterRange:actualRange:),
            NSRange {
                location: 3,
                length: 5,
            },
            &raw mut actual,
        );
        assert_eq!(r.origin.x, 1.0);
        assert_eq!(r.size.height, 4.0);

        let sel_range: unsafe extern "C" fn(Id, Sel) -> NSRange = msg();
        assert_eq!(
            sel_range(obj.as_id(), sel!(selectedRange)),
            NSRange {
                location: 7,
                length: 11
            }
        );

        let marked: unsafe extern "C" fn(Id, Sel, NSRange) -> Bool = msg();
        assert!(
            marked(
                obj.as_id(),
                sel!(setMarkedRange:),
                NSRange {
                    location: 1,
                    length: 0
                }
            )
            .as_bool()
        );
    }
    assert_eq!(
        actual,
        NSRange {
            location: 3,
            length: 5
        },
        "the by-value range did not arrive intact through the out-pointer"
    );
}

// ---------------------------------------------------------------------------
// P3-6 — every block argument and return states its ABI, like everything else.
// ---------------------------------------------------------------------------

#[test]
fn the_three_real_block_shapes_all_satisfy_the_new_encode_bounds() {
    // The whole point of the finding is that this bound costs the tree nothing:
    // all three live sites already satisfy it. `(NSEvent*) -> NSEvent*`,
    // `(NSInteger) -> void`, `(NSRunningApplication*, NSError*) -> void`.
    // SAFETY: none of these blocks is invoked; the constructors are what the
    // bounds are being exercised through.
    unsafe {
        let a = aterm_objc::RcBlock::new1(|e: Id| e).expect("_Block_copy");
        let b = aterm_objc::RcBlock::new1(|_n: i64| {}).expect("_Block_copy");
        let c = aterm_objc::RcBlock::new2(|_app: Id, _err: Id| {}).expect("_Block_copy");
        assert!(!a.as_ptr().is_null() && !b.as_ptr().is_null() && !c.as_ptr().is_null());
    }
    // The negative side — a `*mut bool` argument (`BOOL *` is `signed char *`
    // on the x86_64 compat slice) and a `-> bool` return — are `compile_fail`
    // doctests on the constructors, because a test that does not compile
    // cannot live in a test binary. `tests/blocks.rs` carries the positive
    // form: `enumerateLinesUsingBlock:`'s `stop` is `*mut Bool`, and it used to
    // be `*mut bool` until this bound refused it.
    assert_eq!(
        <*mut Bool as Encode>::ENCODING,
        if cfg!(target_arch = "aarch64") {
            "^B"
        } else {
            "*"
        }
    );
}

// ---------------------------------------------------------------------------
// `abort_on_unwind` used `eprintln!`, which PANICS if stderr is unwritable.
// ---------------------------------------------------------------------------

/// Set in the child: point fd 2 at a pipe with no reader, then panic inside a
/// declared method.
const BROKEN_STDERR: &str = "ATERM_OBJC_P3_BROKEN_STDERR";

declare_class! {
    /// Panics on demand, so the guard runs.
    struct Panicker: NSObject {
        const NAME: &str = "ATermObjcPanickerW2D";
        type Ivars = ();

        @sel(boom)
        fn boom(&self) {
            panic!("deliberate");
        }
    }
}

#[test]
fn the_unwind_guard_does_not_itself_panic_when_stderr_is_gone() {
    // libSystem, already linked into every process this crate runs in — the
    // same "declare the symbol, this crate has no dependencies" discipline the
    // rest of the file uses for the runtime's own entry points.
    unsafe extern "C" {
        fn pipe(fds: *mut i32) -> i32;
        fn dup2(old: i32, new: i32) -> i32;
        fn close(fd: i32) -> i32;
    }

    if std::env::var_os(BROKEN_STDERR).is_some() {
        // Count how many times the panic HOOK runs, on stdout, which stays
        // open. Exactly one panic should happen: the deliberate one. A second
        // means the guard itself panicked trying to report the first — which is
        // what `eprintln!` does on a closed fd 2, and which unwinds out of the
        // very `extern "C"` frame the guard exists to stop, landing on Rust's
        // unnamed "thread caused non-unwinding panic" shim.
        static HOOKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::panic::set_hook(Box::new(|_| {
            let n = HOOKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            println!("HOOK {n}");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }));
        let obj = Panicker::alloc_init(()).expect("+alloc/-init");
        // Point fd 2 at a pipe and then close BOTH ends of it: every write to
        // stderr now fails with `EPIPE` (Rust ignores `SIGPIPE`, so it is an
        // error rather than a signal). MEASURED to be the reliable way to do
        // this — merely closing fd 2 is NOT, because Rust's `Stderr` keeps
        // writing successfully afterwards under the test harness.
        // SAFETY: `fds` is a live two-element array; `dup2`/`close` take plain
        // descriptors and this thread owns fd 2. Nothing after this point may
        // rely on stderr, which is the property under test.
        unsafe {
            let mut fds = [0_i32; 2];
            assert_eq!(pipe(fds.as_mut_ptr()), 0, "pipe()");
            assert_eq!(dup2(fds[1], 2), 2, "dup2 onto stderr");
            assert_eq!(close(fds[0]), 0, "close the read end");
            assert_eq!(close(fds[1]), 0, "close the spare write end");
        }
        // SAFETY: `-boom` is declared on this class with this exact prototype.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = msg();
            f(obj.as_id(), sel!(boom));
        }
        unreachable!("the guard must abort");
    }

    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .arg("the_unwind_guard_does_not_itself_panic_when_stderr_is_gone")
        .arg("--exact")
        .arg("--nocapture")
        .env(BROKEN_STDERR, "1")
        .output()
        .expect("spawn child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let hooks = stdout.matches("HOOK ").count();
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "expected the guard to abort\nstdout: {stdout}"
    );
    assert_eq!(
        hooks, 1,
        "exactly one panic should reach the hook — the deliberate one. MORE \
         means `abort_on_unwind` panicked while trying to report it, which is \
         what `eprintln!` does when stderr cannot be written (measured: three \
         hooks, because the failed-print panic cascades). That unwinds out of \
         the `extern \"C\"` frame the guard exists to stop.\nstdout: {stdout}"
    );
}
