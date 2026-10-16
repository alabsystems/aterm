// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Adversarial tests, each running a counterexample at the spelling the live
// sites use: one encoding letter per pointer-shaped runtime type (`Class` is
// `#`), an ivar at the 16-byte alignment ceiling, `NSRange` by value and by
// pointer, the `Encode` bound on the real block shapes, `BOOL *` read back
// from Foundation, and `abort_on_unwind` with an unwritable stderr.

#![cfg(target_os = "macos")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::ffi::{CStr, c_char, c_void};
use std::os::unix::process::ExitStatusExt as _;
use std::process::Command;

use aterm_objc::{
    Bool, CGRect, ClassPtr, ClassType, Encode, IVAR_NAME, Id, NSRange, ProtocolPtr, Sel,
    declare_class, method_encoding, msg, protocol, sel, sel_uncached,
};

/// The [`MainThread`](aterm_objc::MainThread) witness, minted unchecked:
/// libtest runs every test on a worker, so `MainThread::new()` answers `None`.
/// Every class here touches no AppKit state and is created and released on the
/// same worker, which is `new_unchecked`'s second obligation.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: see this function's doc comment — the class has no main-thread
    // affinity and its ivars are born and dropped on this one worker.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

/// `SIGABRT`, spelled out: this crate has no dependencies, `libc` included.
const SIGABRT: i32 = 6;

unsafe extern "C" {
    fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
    fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
    fn class_getInstanceVariable(cls: ClassPtr, name: *const c_char) -> *const c_void;
    fn ivar_getOffset(ivar: *const c_void) -> isize;
    fn method_copyArgumentType(m: *const c_void, index: u32) -> *mut c_char;
    fn free(p: *mut c_void);
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
// Four pointer-shaped runtime types, four letters. `Id`, `ClassPtr`,
// `ProtocolPtr` and `Sel` are distinct types with their own `Encode` impls: a
// shared `*mut c_void` alias would register KVO's `context:` as an object (`@`)
// and `Class` as `@` where clang emits `#`.
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
    // The letters, from clang:
    //   @encode(id) = @   @encode(Class) = #   @encode(Protocol *) = @
    //   @encode(SEL) = :  @encode(void *) = ^v
    assert_eq!(<Id as Encode>::ENCODING, "@");
    assert_eq!(<ClassPtr as Encode>::ENCODING, "#");
    assert_eq!(<ProtocolPtr as Encode>::ENCODING, "@");
    assert_eq!(<Sel as Encode>::ENCODING, ":");
    assert_eq!(<*mut c_void as Encode>::ENCODING, "^v");

    // Four distinct Rust types make four impls possible; were `Id` an alias of
    // `*mut c_void`, the impls would collide.
    assert_eq!(size_of::<Id>(), size_of::<*mut c_void>());
    assert_eq!(size_of::<ClassPtr>(), size_of::<*mut c_void>());
    assert_eq!(align_of::<Id>(), align_of::<*mut c_void>());

    // What the runtime holds, which is what `NSMethodSignature`,
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
    let obj = Shapes::alloc_init(mtm(), ()).expect("+alloc/-init");
    // SAFETY: each prototype below is exactly the declared method's signature,
    // on a live instance of the class that declares it.
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

// ---------------------------------------------------------------------------
// The 16-byte ivar alignment ceiling (`S4` in the crate docs).
// ---------------------------------------------------------------------------

/// Exactly at the ceiling: 16 bytes, which is what `class_createInstance`'s
/// `malloc` guarantees and therefore what an ivar may ask for.
#[repr(align(16))]
#[derive(Debug, PartialEq, Eq)]
struct AtTheCeiling(u64, u64);

declare_class! {
    /// The largest alignment an ivar may have; `align(32)` and up are refused
    /// (the `compile_fail` doctest on
    /// [`aterm_objc::ClassBuilder::add_rust_ivar`]).
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
    // `class_addIvar` aligns the ivar's offset, but the instance base is only
    // `malloc`'s 16-aligned (measurements on `ClassBuilder::add_rust_ivar`), so
    // the slot's real alignment is `min(16, requested)`. Checked over 4,096 raw
    // `+alloc`s, with no Rust reference in the loop; the base count shows the
    // base really is only 16-aligned.
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
        let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = msg();
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

    // End to end through the safe `ivars()` accessor, which can discharge
    // `IvarSlot::get`'s alignment precondition because no over-aligned class
    // exists.
    let obj = Aligned16::alloc_init(mtm(), AtTheCeiling(20, 22)).expect("+alloc/-init");
    // SAFETY: `-sum` is declared on this class with exactly this prototype.
    let sum = unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> i64 = msg();
        f(obj.as_id(), sel!(sum))
    };
    assert_eq!(sum, 42);
    assert_eq!(obj.ivars(), &AtTheCeiling(20, 22));
}

// ---------------------------------------------------------------------------
// `NSRange` by value and by pointer, as winit's `NSTextInputClient` needs.
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
                // SAFETY: `actualRange:` is an out parameter; the framework
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
    // From clang, a compiled `@implementation` of the same declarations:
    //
    //   firstRectForCharacterRange:actualRange:
    //     {CGRect={CGPoint=dd}{CGSize=dd}}40@0:8{_NSRange=QQ}16^{_NSRange=QQ}32
    //
    // The struct tag is `_NSRange` (`NSRange` is a typedef) and `Q` is
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

    let obj = TextInput::alloc_init(mtm(), ()).expect("+alloc/-init");
    let mut actual = NSRange::default();
    // SAFETY: the three prototypes are exactly the declared signatures. The
    // out-pointer addresses a live local for the duration of the send.
    unsafe {
        let rect: unsafe extern "C-unwind" fn(Id, Sel, NSRange, *mut NSRange) -> CGRect = msg();
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

        let sel_range: unsafe extern "C-unwind" fn(Id, Sel) -> NSRange = msg();
        assert_eq!(
            sel_range(obj.as_id(), sel!(selectedRange)),
            NSRange {
                location: 7,
                length: 11
            }
        );

        let marked: unsafe extern "C-unwind" fn(Id, Sel, NSRange) -> Bool = msg();
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
// Every block argument and return states its ABI.
// ---------------------------------------------------------------------------

/// `BOOL *`'s encoding, asked of the runtime: argument 3 of
/// `-[NSFileManager fileExistsAtPath:isDirectory:]` is its `BOOL *isDirectory`,
/// encoded by clang when Apple built Foundation (`c32@0:8@16^c24` on x86_64).
/// A pin against a typed literal stays green when the constant and the literal
/// are wrong together; this one cannot.
#[test]
fn bool_ptr_encodes_what_foundation_compiled_for_its_bool_out_parameter() {
    let cls = aterm_objc::class(c"NSFileManager");
    assert!(!cls.is_null(), "Foundation's NSFileManager is registered");
    let name = c"fileExistsAtPath:isDirectory:";
    // SAFETY: side-effect-free runtime queries on a registered class.
    // `method_copyArgumentType` answers a `malloc`ed copy (null past the last
    // argument), which is read and then freed exactly once.
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

// ---------------------------------------------------------------------------
// `abort_on_unwind` must not panic when stderr is unwritable, as `eprintln!`
// would.
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
    // libSystem, linked into every process; declared here because this crate
    // has no dependencies.
    unsafe extern "C" {
        fn pipe(fds: *mut i32) -> i32;
        fn dup2(old: i32, new: i32) -> i32;
        fn close(fd: i32) -> i32;
    }

    if std::env::var_os(BROKEN_STDERR).is_some() {
        // Count panic-hook runs on stdout, which stays open. Exactly one panic
        // should happen; a second means the guard panicked reporting the first
        // (as `eprintln!` does on a broken fd 2), unwinding out of the
        // `extern "C"` frame it exists to protect.
        static HOOKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::panic::set_hook(Box::new(|_| {
            let n = HOOKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            println!("HOOK {n}");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }));
        let obj = Panicker::alloc_init(mtm(), ()).expect("+alloc/-init");
        // Point fd 2 at a pipe and close both ends: every stderr write then
        // fails with `EPIPE` (Rust ignores `SIGPIPE`). Merely closing fd 2 is
        // not enough: Rust's `Stderr` keeps succeeding under the test harness.
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
            let f: unsafe extern "C-unwind" fn(Id, Sel) = msg();
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
