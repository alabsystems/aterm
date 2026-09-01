// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// W2 second pass, part three: the nine findings, each armed with the judge's
// own counterexample.
//
// | finding | the counterexample this file runs                                  |
// |---------|--------------------------------------------------------------------|
// | F1      | a 14-colon selector — 16 parameters — declared, registered, SENT    |
// | F2      | both safe `autorelease` wrappers, with NO pool, in a child process  |
// | F3      | a panicking `Drop` inside a block's captures, in a child process    |
// | F6      | the packed 9-byte return type, and the 9-byte one that is NOT       |
// | F7      | the KVO `context:` shape, encoding read back out of the runtime     |
// | F8      | a block returning a type with no `Default` impl                     |
// | F9      | this file compiles with NO crate-level `too_many_arguments` allow   |

#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};
use std::os::unix::process::ExitStatusExt as _;
use std::process::Command;

use aterm_objc::{
    ClassPtr, ClassType, Encode, Id, Obj, RcBlock, Retained, Sel, class, declare_class,
    method_encoding, msg, ns_string, sel, sel_uncached,
};

/// `SIGABRT`, spelled out: this crate has ZERO dependencies by construction.
const SIGABRT: i32 = 6;

/// The type encoding the runtime holds for `cls`'s `name`, offsets and all.
fn encoding_of(cls: ClassPtr, name: &'static CStr) -> String {
    unsafe extern "C" {
        fn class_getInstanceMethod(cls: ClassPtr, sel: Sel) -> *const c_void;
        fn method_getTypeEncoding(m: *const c_void) -> *const c_char;
    }
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
// F1 — the arity wall was 10 arguments, and only one end of it was documented.
// ---------------------------------------------------------------------------

declare_class! {
    /// FOURTEEN colons — 16 parameters counting `self` and `_cmd`, the new
    /// ceiling of [`aterm_objc::MsgFn`]. Before this wave `MsgFn` stopped at
    /// twelve parameters, so this method would have DECLARED, REGISTERED and
    /// dispatched from Objective-C while `msg()` refused to build a prototype
    /// for it (`E0277`) — a method the runtime can reach and Rust cannot. That
    /// is D1's arity trap one level up.
    ///
    /// Note also that no `#[allow(clippy::too_many_arguments)]` appears at this
    /// call site or at the top of this file: the macro emits it, which is F9.
    struct Widest: NSObject {
        const NAME: &str = "ATermObjcWidestW2C";
        type Ivars = ();

        @sel(a:b:c:d:e:f:g:h:i:j:k:l:m:n:)
        fn widest(
            &self,
            a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64,
            h: i64, i: i64, j: i64, k: i64, l: i64, m: i64, n: i64,
        ) -> i64 {
            // Each argument is weighted by its POSITION, so a send that shifts
            // by one register cannot produce this sum by accident.
            a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7
                + h * 8 + i * 9 + j * 10 + k * 11 + l * 12 + m * 13 + n * 14
        }
    }
}

#[test]
fn a_fourteen_argument_method_is_sendable_from_rust_not_just_declarable() {
    let obj = Widest::alloc_init(()).expect("+alloc/-init");
    // SAFETY: the exact declared prototype — 16 parameters, `self` and `_cmd`
    // included — on a live instance of the class that declares it.
    unsafe {
        #[rustfmt::skip]
        let f: unsafe extern "C" fn(
            Id, Sel,
            i64, i64, i64, i64, i64, i64, i64,
            i64, i64, i64, i64, i64, i64, i64,
        ) -> i64 = msg();
        let s = sel!(a:b:c:d:e:f:g:h:i:j:k:l:m:n:);
        // 1+2+…+14 = 105.
        assert_eq!(
            f(obj.as_id(), s, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1),
            105
        );
        // Only the FIRST argument set: 1. A one-register shift would read
        // `_cmd` here and give something else entirely.
        assert_eq!(
            f(obj.as_id(), s, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
            1
        );
        // Only the LAST: 14. This is the argument that falls off the end of the
        // register file on both ABIs, so it is the one a stack-slot mistake
        // corrupts first.
        assert_eq!(
            f(obj.as_id(), s, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1),
            14
        );
    }
    // The runtime agrees on the arity: fourteen `q`s after `@:`.
    assert_eq!(
        encoding_of(Widest::class(), c"a:b:c:d:e:f:g:h:i:j:k:l:m:n:"),
        "q@:qqqqqqqqqqqqqq"
    );
}

// ---------------------------------------------------------------------------
// F2 — a `# Safety` clause that safe code in the same crate violated by
// construction. The fix is to demote the clause, so the arming is a child
// process that does the violating thing and does NOT die.
// ---------------------------------------------------------------------------

/// Set in the child so it runs the body instead of re-spawning.
const CHILD_NO_POOL: &str = "ATERM_OBJC_AUTORELEASE_NO_POOL_CHILD";

declare_class! {
    /// A class to hand `Retained::autorelease` — the typed twin of the wrapper
    /// on `Obj`, which is the second safe caller of the same function.
    struct Bare: NSObject {
        const NAME: &str = "ATermObjcBareW2C";
        type Ivars = ();
    }
}

#[test]
fn autoreleasing_with_no_pool_leaks_and_that_is_all_it_does() {
    if std::env::var_os(CHILD_NO_POOL).is_some() {
        // NOT inside `autoreleasepool`, and nothing above this frame pushed a
        // pool either — a libtest worker thread starts with an empty pool
        // stack. Both safe wrappers run here. Under the OLD `# Safety` text
        // ("There must be a pool on this thread's stack") both of these lines
        // were unsound-by-contract, and neither of them can uphold it, because
        // neither can see its caller's stack.
        let s = ns_string("no pool anywhere").expect("NSString");
        let borrowed: Id = s.autorelease();
        assert!(!borrowed.is_null());

        let typed = Bare::alloc_init(()).expect("+alloc/-init");
        let borrowed2: Id = typed.autorelease();
        assert!(!borrowed2.is_null());

        // The object is not merely un-crashed, it is ALIVE: the runtime leaked
        // it rather than freeing it, so a message still lands. If the +1 had
        // been dropped this would be a use-after-free and the child would die,
        // which is the outcome the parent asserts against.
        // SAFETY: `-length` on a live NSString, whose prototype this is.
        let len = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
            f(borrowed, sel!(length))
        };
        assert_eq!(len, "no pool anywhere".len());
        // SAFETY: `-hash` on a live NSObject.
        let _ = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
            f(borrowed2, sel!(hash))
        };

        println!("SURVIVED-THE-POOLLESS-AUTORELEASE");
        return;
    }

    let run = |debug_pools: bool| {
        let exe = std::env::current_exe().expect("the test binary's own path");
        let mut cmd = Command::new(exe);
        cmd.args([
            "--exact",
            "--nocapture",
            "autoreleasing_with_no_pool_leaks_and_that_is_all_it_does",
        ])
        .env(CHILD_NO_POOL, "1");
        if debug_pools {
            cmd.env("OBJC_DEBUG_MISSING_POOLS", "YES");
        }
        let out = cmd
            .output()
            .expect("re-running this test binary as a child");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(
            out.status.signal(),
            None,
            "a pool-less autorelease killed the process; the demoted contract \
             in `runtime::autorelease` would be WRONG and must be restored\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            out.status.success(),
            "the child exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            stdout.contains("SURVIVED-THE-POOLLESS-AUTORELEASE"),
            "the child never reached the end of the body\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
        stderr
    };

    // Default: the runtime says NOTHING. The old `# Safety` text promised the
    // "just leaking" diagnostic unconditionally; it is not printed unless the
    // environment asks for it, which is worth knowing when hunting one of
    // these leaks.
    let quiet = run(false);
    assert!(
        !quiet.contains("autoreleased with no pool"),
        "libobjc now warns about a missing pool by default; the note in \
         `runtime::autorelease` says it does not, and must be re-measured\n\
         {quiet}"
    );

    // Asked for: the diagnostic the note quotes, verbatim, and still exit 0.
    let loud = run(true);
    assert!(
        loud.contains("MISSING POOLS")
            && loud.contains("autoreleased with no pool in place")
            && loud.contains("just leaking"),
        "OBJC_DEBUG_MISSING_POOLS no longer produces the quoted diagnostic; \
         the note in `runtime::autorelease` must be re-measured\n{loud}"
    );
}

// ---------------------------------------------------------------------------
// F3 — the one `extern "C"` callback in the crate with no panic guard.
// ---------------------------------------------------------------------------

/// Set in the child so it runs the body instead of re-spawning.
const CHILD_DISPOSE: &str = "ATERM_OBJC_BLOCK_DISPOSE_PANIC_CHILD";

/// A capture whose `Drop` panics — the only way to reach `dispose_helper` with
/// an unwind, since `dispose` runs nothing else of the user's.
struct PanicsOnDrop;
impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("deliberate panic from a block capture's Drop");
    }
}

#[test]
fn a_panicking_drop_in_a_block_capture_aborts_by_name() {
    if std::env::var_os(CHILD_DISPOSE).is_some() {
        let bomb = PanicsOnDrop;
        // SAFETY: a no-argument block that is never invoked; the only thing
        // this test needs from it is that releasing it runs `dispose_helper`,
        // which drops the capture.
        let block = unsafe {
            RcBlock::new0(move || {
                let _keep = &bomb;
            })
        }
        .expect("_Block_copy");
        // Last release -> `dispose_helper` -> `drop_in_place` -> panic.
        drop(block);
        println!("SURVIVED-THE-PANICKING-DISPOSE");
        return;
    }

    let exe = std::env::current_exe().expect("the test binary's own path");
    let out = Command::new(exe)
        .args([
            "--exact",
            "--nocapture",
            "a_panicking_drop_in_a_block_capture_aborts_by_name",
        ])
        .env(CHILD_DISPOSE, "1")
        .output()
        .expect("re-running this test binary as a child");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("SURVIVED-THE-PANICKING-DISPOSE"),
        "the unwind escaped `dispose_helper` without aborting\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "expected SIGABRT\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The whole of F3: it aborted BEFORE, too — through Rust's own
    // `extern "C"` shim, with "thread caused non-unwinding panic. aborting.",
    // which names neither this crate nor this callback. The guard makes it say
    // which one, exactly as `invoke`, `__tramp` and `__dealloc` already did.
    assert!(
        stderr.contains("panic escaped Objective-C method `block dispose`"),
        "aborted, but not through `abort_on_unwind` with the callback's \
         name — the convention has an exception again:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// F7 — every `*const c_void` in a signature encoded as `":"`, a SELECTOR.
// ---------------------------------------------------------------------------

declare_class! {
    /// The shapes that used to be the SAME Rust type, side by side.
    ///
    /// `observeValueForKeyPath:ofObject:change:context:` is KVO's callback, the
    /// method W2 needs for winit. Its last argument is spelled `void *` — NOT
    /// `const void *` — in the SDK (`NSKeyValueObserving.h`:
    /// `context:(nullable void *)context`), in `objc2-foundation`'s generated
    /// binding (`context: *mut c_void`) and in
    /// `vendor/winit/src/platform_impl/macos/window_delegate.rs:456`
    /// (`_context: *mut c_void`). THE THIRD PASS MOVED IT TO THAT SPELLING:
    /// this arming used to write `*const c_void`, which mapped to `"^v"` and
    /// was green while the `*mut` spelling every live site uses still mapped to
    /// `"@"` — an OBJECT, which invites `NSInvocation` and forwarding to retain
    /// a pointer the caller owns. A guard armed against a shape nobody will
    /// ever port is not a guard.
    ///
    /// `control:textView:doCommandBySelector:` is the one place in the tree
    /// that really does take a `SEL`; `takeConstContext:` keeps the `const`
    /// spelling covered too.
    struct Observer: NSObject {
        const NAME: &str = "ATermObjcObserverW2C";
        type Ivars = ();

        @sel(observeValueForKeyPath:ofObject:change:context:)
        fn observe(&self, _key: Id, _obj: Id, _change: Id, context: *mut c_void) {
            assert!(context.is_null() || !context.is_null());
        }

        @sel(takeConstContext:)
        fn take_const_context(&self, context: *const c_void) {
            assert!(context.is_null() || !context.is_null());
        }

        @sel(control:textView:doCommandBySelector:)
        fn do_command(&self, _c: Id, _tv: Id, command: Sel) -> aterm_objc::Bool {
            aterm_objc::Bool::new(!command.is_null())
        }
    }
}

#[test]
fn a_context_pointer_and_a_selector_no_longer_encode_the_same() {
    // The static half: the encodings differ, and each is the runtime's own
    // spelling. `^v` is "pointer to void"; `:` is `SEL`; `@` is now `Id`'s
    // alone, and `Id` is a newtype rather than an alias of `*mut c_void`.
    assert_eq!(<*mut c_void as Encode>::ENCODING, "^v");
    assert_eq!(<*const c_void as Encode>::ENCODING, "^v");
    assert_eq!(<Sel as Encode>::ENCODING, ":");
    assert_eq!(<Id as Encode>::ENCODING, "@");
    assert_eq!(method_encoding!(() ; Id, Id, Id, *mut c_void), "v@:@@@^v");
    assert_eq!(method_encoding!(() ; Id, Id, Id, *const c_void), "v@:@@@^v");
    assert_eq!(
        method_encoding!(aterm_objc::Bool ; Id, Id, Sel),
        format!("{}@:@@:", aterm_objc::Bool::ENCODING)
    );

    // The measured half: what the RUNTIME holds for the registered methods,
    // which is what `NSMethodSignature`, `NSInvocation`, forwarding and
    // accessibility all read. `v@:@@@:` — the old, wrong string — would have
    // told AppKit the context pointer was a selector.
    let cls = Observer::class();
    assert_eq!(
        encoding_of(cls, c"observeValueForKeyPath:ofObject:change:context:"),
        "v@:@@@^v"
    );
    // Which is byte-for-byte what clang emits for the same declaration, less
    // the offsets the short form omits: `v48@0:8@16@24@32^v40`. MEASURED on
    // this box with `method_getTypeEncoding` over a compiled `@implementation`.
    assert_eq!(encoding_of(cls, c"takeConstContext:"), "v@:^v");
    assert_eq!(
        encoding_of(cls, c"control:textView:doCommandBySelector:"),
        format!("{}@:@@:", aterm_objc::Bool::ENCODING)
    );

    // And the newtype is still exactly the word the runtime passes.
    assert_eq!(size_of::<Sel>(), size_of::<*const c_void>());
    assert_eq!(align_of::<Sel>(), align_of::<*const c_void>());
    assert_eq!(
        sel!(length).as_ptr(),
        sel_uncached(c"length").as_ptr(),
        "interning is idempotent, so the cached and uncached forms are one \
         pointer"
    );
    assert!(!sel!(length).is_null());

    // The newtype gained a `Debug`, and it goes through `sel_getName` rather
    // than casting the pointer to a `char *` — the latter works on `objc4` and
    // is not an ABI this crate is entitled to.
    assert_eq!(
        format!("{:?}", sel!(initWithBytes:length:encoding:)),
        "Sel(initWithBytes:length:encoding:)"
    );
    // The empty selector interns to a real, non-null SEL with an empty name —
    // which is why `Debug`'s null arm cannot be reached from safe code, and why
    // `Sel::from_ptr` is `pub(crate)`.
    assert!(!sel_uncached(c"").is_null());
    assert_eq!(format!("{:?}", sel_uncached(c"")), "Sel()");
}

// ---------------------------------------------------------------------------
// F8 — `R: Default` was dead weight on every block constructor.
// ---------------------------------------------------------------------------

/// No `Default`, deliberately, and no way to add one that would matter: this is
/// the shape of every real block return that is not `()` — an owned handle.
///
/// It DOES implement [`Encode`], which is new in the third pass: `RcBlock`'s
/// constructors now carry the same `Encode` bound `msg` carries on its return
/// type, so a block's arguments and return state their C ABI like everything
/// else that crosses a boundary in this crate. That bound is orthogonal to F8's
/// point — F8 is about `Default`, which is still absent, and the constructor
/// still accepts this type.
#[repr(transparent)]
#[derive(Debug, PartialEq, Eq)]
struct NoDefault(u64);

// SAFETY: `#[repr(transparent)]` over one `u64`, so its C ABI is exactly
// `unsigned long long`'s and nothing is packed.
unsafe impl Encode for NoDefault {
    const ENCODING: &'static str = "Q";
}

#[test]
fn a_block_may_return_a_type_that_has_no_default_impl() {
    // Before this wave the bound `R: Default` on every `RcBlock::newN` made
    // this line `E0277`, for a bound the body never used: the caught-panic arm
    // lands on `abort_on_unwind`, which is `!` and coerces to any `R`.
    // SAFETY: the closure's prototype is exactly what `invoke` is typed as, and
    // the block is invoked below through that same prototype.
    let block = unsafe { RcBlock::new0(|| NoDefault(0xFEED)) }.expect("_Block_copy");

    // SAFETY: the block ABI puts the block itself in the first argument slot
    // and `invoke` at a fixed offset in the header; this is the prototype the
    // constructor installed.
    let got = unsafe {
        #[repr(C)]
        struct Header {
            _isa: *const c_void,
            _flags: i32,
            _reserved: i32,
            invoke: *const c_void,
        }
        let invoke = (*block.as_ptr().cast::<Header>()).invoke;
        let f: unsafe extern "C" fn(*mut c_void) -> NoDefault = std::mem::transmute(invoke);
        f(block.as_ptr())
    };
    assert_eq!(got, NoDefault(0xFEED));

    // One argument and two, same absence of a bound.
    // SAFETY: as above.
    let one = unsafe { RcBlock::new1(|n: u64| NoDefault(n)) }.expect("_Block_copy");
    // SAFETY: as above.
    let two = unsafe { RcBlock::new2(|a: u64, b: u64| NoDefault(a + b)) }.expect("_Block_copy");
    assert!(!one.as_ptr().is_null() && !two.as_ptr().is_null());
}

// ---------------------------------------------------------------------------
// F6's structural half lives in `tests/abi.rs`, where the rest of the
// return-classification evidence is. What belongs here is the API consequence:
// a return type nobody has stated an ABI for cannot be sent at all.
// ---------------------------------------------------------------------------

#[test]
fn the_return_types_this_crate_can_send_all_declare_their_abi() {
    // Every one of these has an `Encode` impl with a SAFETY comment naming its
    // C ABI, which is now a PRECONDITION of `msg` rather than a courtesy.
    // SAFETY: no pointer here is called; the test only builds the prototypes,
    // which is what exercises the bound.
    unsafe {
        let _: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let _: unsafe extern "C" fn(Id, Sel) = msg();
        let _: unsafe extern "C" fn(Id, Sel) -> aterm_objc::Bool = msg();
        let _: unsafe extern "C" fn(Id, Sel) -> aterm_objc::CGRect = msg();
        let _: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
        let _: unsafe extern "C" fn(Id, Sel) -> Sel = msg();
        let _: unsafe extern "C" fn(Id, Sel) -> *const c_void = msg();
    }
    // A real send through the newly-encodable `*const c_char`: `-UTF8String`.
    let s = ns_string("utf8 through a char*").expect("NSString");
    // SAFETY: `-UTF8String` on a live NSString returns a NUL-terminated buffer
    // owned by the receiver and valid while it is.
    let round = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
        CStr::from_ptr(f(s.id(), sel!(UTF8String)))
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(round, "utf8 through a char*");

    // The negative side is a `compile_fail` doctest on `msg` itself, because a
    // test that does not compile cannot live in a test binary.
    let _ = (class(c"NSObject"), Obj::retain, Retained::<Bare>::retain);
}
