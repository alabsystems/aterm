// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Subprocess tests for fatal and non-fatal Objective-C boundary misuse paths.

#![cfg(target_os = "macos")]
#![expect(
    clippy::disallowed_methods,
    reason = "the subject is the send itself: these tests cast the bare `msg` by hand"
)]

use aterm_objc::{
    ClassPtr, ClassType, Id, RcBlock, Sel, class, declare_class, msg, ns_string, sel,
};

use crate::reexec;

/// The [`MainThread`](aterm_objc::MainThread) witness, minted unchecked.
fn mtm() -> aterm_objc::MainThread {
    // SAFETY: see this function's doc comment.
    unsafe { aterm_objc::MainThread::new_unchecked() }
}

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
    /// Reached by name in the child.
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
    /// A class to hand `Retained::autorelease`.
    struct Bare: NSObject {
        const NAME: &str = "ATermObjcPoollessProbe";
        type Ivars = ();
    }
}

/// Set in the child so it performs the panicking send instead of re-spawning.
const PANIC_CHILD: &str = "ATERM_OBJC_PANIC_CHILD";

#[test]
fn a_panic_inside_a_declared_method_aborts_instead_of_unwinding_into_foundation() {
    if reexec::is_child(PANIC_CHILD) {
        let obj = Panicker::alloc_init(mtm(), ()).expect("+alloc/-init");
        // SAFETY: `-performSelector:` is `-(id)performSelector:(SEL)`.
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
        out.status.code(),
        Some(reexec::ABORTED),
        "expected an abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("panic escaped Objective-C method `boom`"),
        "aborted, but not through aterm-objc's own guard:\n{stderr}"
    );
}

/// Set in the child.
const BROKEN_STDERR: &str = "ATERM_OBJC_P3_BROKEN_STDERR";

#[test]
fn the_unwind_guard_does_not_itself_panic_when_stderr_is_gone() {
    unsafe extern "C" {
        fn pipe(fds: *mut i32) -> i32;
        fn dup2(old: i32, new: i32) -> i32;
        fn close(fd: i32) -> i32;
    }

    if reexec::is_child(BROKEN_STDERR) {
        static HOOKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::panic::set_hook(Box::new(|_| {
            let n = HOOKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            println!("HOOK {n}");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }));
        let obj = Panicker::alloc_init(mtm(), ()).expect("+alloc/-init");
        // SAFETY: `fds` is a live two-element array.
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
        out.status.code(),
        Some(reexec::ABORTED),
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

/// Set in the child so it mints an instance the way Objective-C would.
const OBJC_MINTED_CHILD: &str = "ATERM_OBJC_MINTED_CHILD";

#[test]
fn an_instance_objective_c_minted_is_caught_by_the_ivar_flag() {
    if reexec::is_child(OBJC_MINTED_CHILD) {
        // SAFETY: `+alloc` and `-init` on a registered class reached by name.
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
        out.status.code(),
        Some(reexec::ABORTED),
        "expected the named abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("ivars read before they were written"),
        "aborted, but not through the ivar flag:\n{stderr}"
    );
}

/// Set in the child so it runs the body instead of re-spawning.
const CHILD_DISPOSE: &str = "ATERM_OBJC_BLOCK_DISPOSE_PANIC_CHILD";

/// A capture whose `Drop` panics.
struct PanicsOnDrop;
impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("deliberate panic from a block capture's Drop");
    }
}

#[test]
fn a_panicking_drop_in_a_block_capture_aborts_by_name() {
    if reexec::is_child(CHILD_DISPOSE) {
        let bomb = PanicsOnDrop;
        // SAFETY: a no-argument block that is never invoked.
        let block = unsafe {
            RcBlock::new0(move || {
                let _keep = &bomb;
            })
        }
        .expect("_Block_copy");
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
        out.status.code(),
        Some(reexec::ABORTED),
        "expected an abort\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("panic escaped Objective-C method `block dispose`"),
        "aborted, but not through `abort_on_unwind` with the callback's \
         name — the convention has an exception again:\n{stderr}"
    );
}

/// Set in the child so it runs the body instead of re-spawning.
const CHILD_NO_POOL: &str = "ATERM_OBJC_AUTORELEASE_NO_POOL_CHILD";

#[test]
fn autoreleasing_with_no_pool_leaks_and_that_is_all_it_does() {
    if reexec::is_child(CHILD_NO_POOL) {
        let s = ns_string("no pool anywhere").expect("NSString");
        let borrowed: Id = s.autorelease();
        assert!(!borrowed.is_null());

        let typed = Bare::alloc_init(mtm(), ()).expect("+alloc/-init");
        let borrowed2: Id = typed.autorelease();
        assert!(!borrowed2.is_null());

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
        out.status.code(),
        Some(0),
        "a pool-less autorelease killed the process; the demoted contract \
         in `runtime::autorelease` would be WRONG and must be restored\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        out.status.success() && stdout.contains("SURVIVED-THE-POOLLESS-AUTORELEASE"),
        "the child did not reach the end of the body\nstdout: {stdout}\nstderr: {stderr}"
    );
}
