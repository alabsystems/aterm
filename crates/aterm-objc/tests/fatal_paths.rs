// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The misuses that must END the process by name, and the one that must not.
//! Each runs its fatal body in a child (this binary re-run on the one test,
//! `support/reexec.rs`) and reads the signal and the stderr back:
//!
//! * a Rust panic inside a declared method that Foundation dispatched aborts
//!   with the method's name instead of unwinding into Foundation;
//! * the guard that reports it does not itself panic when stderr is gone;
//! * an instance Objective-C minted by name, never through `alloc_init`, is
//!   caught by the ivar-written flag before an all-zero slot is read;
//! * a panicking `Drop` inside a block's captures aborts naming the block's
//!   dispose helper;
//! * and autoreleasing with no pool on the thread only LEAKS: the child must
//!   survive it, which is what lets `autorelease`'s contract be safe.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "the subject is the send itself: these tests cast the bare `msg` by hand"
)]

use std::os::unix::process::ExitStatusExt as _;

use aterm_objc::{
    ClassPtr, ClassType, Id, RcBlock, Sel, class, declare_class, msg, ns_string, sel,
};

use crate::reexec;

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

declare_class! {
    /// Panics on demand, so the unwind guard runs.
    struct Panicker: NSObject {
        const NAME: &str = "ATermObjcPanicProbe";
        type Ivars = ();

        @sel(boom)
        fn boom(&self) {
            panic!("a declared method panicked");
        }
    }
}

/// The ivars an Objective-C-minted instance never had written.
struct MintedIvars {
    marker: u64,
}

declare_class! {
    /// Reached by name in the child, the way a nib or `NSClassFromString`
    /// reaches a class, so `alloc_init` never writes its ivars.
    struct Minted: NSObject {
        const NAME: &str = "ATermObjcMintedProbe";
        type Ivars = MintedIvars;

        @sel(marker)
        fn marker(&self) -> u64 {
            self.ivars().marker
        }
    }
}

declare_class! {
    /// A class to hand `Retained::autorelease` — the typed twin of the wrapper
    /// on `Obj`, which is the second safe caller of the same function.
    struct Bare: NSObject {
        const NAME: &str = "ATermObjcPoollessProbe";
        type Ivars = ();
    }
}

// ---------------------------------------------------------------------------
// A panic inside framework dispatch.
// ---------------------------------------------------------------------------

/// Set in the child so it performs the panicking send instead of re-spawning.
const PANIC_CHILD: &str = "ATERM_OBJC_PANIC_CHILD";

#[test]
fn a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation() {
    if std::env::var_os(PANIC_CHILD).is_some() {
        let obj = Panicker::alloc_init(mtm(), ()).expect("+alloc/-init");
        // Dispatched by Foundation, not by a direct call: `-performSelector:`
        // puts a framework frame between the send and our trampoline, the
        // shape `NSNotificationCenter` and AppKit's responder chain use.
        // SAFETY: `-performSelector:` is `-(id)performSelector:(SEL)`, and the
        // target is a live instance that declares `-boom`.
        unsafe {
            let perform: unsafe extern "C-unwind" fn(Id, Sel, Sel) -> Id = msg();
            perform(obj.as_id(), sel!(performSelector:), sel!(boom));
        }
        println!("SURVIVED-THE-PANIC");
        return;
    }
    let out = reexec::rerun(
        module_path!(),
        "a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation",
        &[(PANIC_CHILD, "1")],
    );
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

/// Set in the child: point fd 2 at a pipe with no reader, then panic inside a
/// declared method.
const BROKEN_STDERR: &str = "ATERM_OBJC_P3_BROKEN_STDERR";

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

    let out = reexec::rerun(
        module_path!(),
        "the_unwind_guard_does_not_itself_panic_when_stderr_is_gone",
        &[(BROKEN_STDERR, "1")],
    );
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

// ---------------------------------------------------------------------------
// An instance Objective-C minted, caught by the ivar-written flag.
// ---------------------------------------------------------------------------

/// Set in the child so it mints an instance the way Objective-C would.
const OBJC_MINTED_CHILD: &str = "ATERM_OBJC_MINTED_CHILD";

#[test]
fn an_instance_objective_c_minted_is_caught_by_the_ivar_flag() {
    // The initialised flag is checked in release builds too: ObjC code can
    // `+alloc`/`-init` the class by name without reaching `alloc_init`, leaving
    // an all-zero ivar slot. The assert turns that into a named abort.
    if std::env::var_os(OBJC_MINTED_CHILD).is_some() {
        // SAFETY: `+alloc` and `-init` on a registered class reached by name,
        // then a message the class declares: what a nib or
        // `NSClassFromString` does, and what the flag guards.
        let _ = Minted::class();
        unsafe {
            let cls: ClassPtr = class(c"ATermObjcMintedProbe");
            assert!(!cls.is_null());
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = msg();
            let raw = alloc(cls, sel!(alloc));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            let obj = init(raw, sel!(init));
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> u64 = msg();
            let v = f(obj, sel!(marker));
            println!("SURVIVED-THE-UNWRITTEN-IVAR v={v}");
        }
        return;
    }
    // Force the class to exist in the child by touching it here too.
    let _ = Minted::class();
    let out = reexec::rerun(
        module_path!(),
        "an_instance_objective_c_minted_is_caught_by_the_ivar_flag",
        &[(OBJC_MINTED_CHILD, "1")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("SURVIVED-THE-UNWRITTEN-IVAR"),
        "an unwritten ivar slot was read as a live value\nstdout: {stdout}"
    );
    assert_eq!(
        out.status.signal(),
        Some(SIGABRT),
        "expected the named abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("ivars read before they were written"),
        "aborted, but not through the ivar flag:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// A panicking `Drop` inside a block's captures: the one `extern "C"` callback
// that once had no panic guard.
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

    let out = reexec::rerun(
        module_path!(),
        "a_panicking_drop_in_a_block_capture_aborts_by_name",
        &[(CHILD_DISPOSE, "1")],
    );

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
    // It aborted BEFORE the guard, too — through Rust's own
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
// Autoreleasing with no pool: a leak, and nothing worse. `autorelease`'s
// contract says so, and this is the arming: a child that does it and lives.
// ---------------------------------------------------------------------------

/// Set in the child so it runs the body instead of re-spawning.
const CHILD_NO_POOL: &str = "ATERM_OBJC_AUTORELEASE_NO_POOL_CHILD";

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

        let typed = Bare::alloc_init(mtm(), ()).expect("+alloc/-init");
        let borrowed2: Id = typed.autorelease();
        assert!(!borrowed2.is_null());

        // The object is not merely un-crashed, it is ALIVE: the runtime leaked
        // it rather than freeing it, so a message still lands. If the +1 had
        // been dropped this would be a use-after-free and the child would die,
        // which is the outcome the parent asserts against.
        // SAFETY: `-length` on a live NSString, whose prototype this is.
        let len = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
            f(borrowed, sel!(length))
        };
        assert_eq!(len, "no pool anywhere".len());
        // SAFETY: `-hash` on a live NSObject.
        let _ = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
            f(borrowed2, sel!(hash))
        };

        println!("SURVIVED-THE-POOLLESS-AUTORELEASE");
        return;
    }

    let out = reexec::rerun(
        module_path!(),
        "autoreleasing_with_no_pool_leaks_and_that_is_all_it_does",
        &[(CHILD_NO_POOL, "1")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.signal(),
        None,
        "a pool-less autorelease killed the process; the demoted contract \
         in `runtime::autorelease` would be WRONG and must be restored\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        out.status.success() && stdout.contains("SURVIVED-THE-POOLLESS-AUTORELEASE"),
        "the child did not reach the end of the body\nstdout: {stdout}\nstderr: {stderr}"
    );
}
