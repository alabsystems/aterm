// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISPATCH DRIVER: `aterm_objc::run_on_main`, and the DEADLOCK it exists
//! to avoid, PROVED by hanging a process on purpose.
//!
//! # Why this is a driver and not a test
//!
//! `run_on_main` cannot be exercised by `libtest` at all, in either direction,
//! and the reason is worth stating because it is the same reason the other four
//! drivers exist:
//!
//! * A libtest binary runs each test on a WORKER thread, so
//!   `+[NSThread isMainThread]` is false and the direct branch is never taken.
//! * That leaves the dispatched branch — which submits to the main queue and
//!   blocks. A libtest binary's main thread is parked joining workers; it never
//!   services its queue. The submission would therefore block until the harness
//!   times out, i.e. the test would HANG rather than fail.
//!
//! A binary whose `main` really is the process main thread, and which can drive
//! a run loop, is the only place either branch is reachable. That is this file.
//!
//! # THE PROOF OBLIGATION, and how it is discharged
//!
//! W10's brief: *"dispatch_sync to the main queue FROM the main thread
//! deadlocks, so the main-thread case must be a direct call, and that must be
//! proved, not asserted."*
//!
//! Asserting it in a comment proves nothing, and a test that merely calls
//! `run_on_main` on the main thread and sees it return proves only that
//! SOMETHING returned — it cannot tell a correct direct call from a
//! `dispatch_sync` that happened not to deadlock. The evidence has to be
//! DIFFERENTIAL, and it has to include the hang:
//!
//! * **Stage 2** re-executes THIS BINARY as a child in `naive` mode, where the
//!   child does the obvious wrong thing — `dispatch_sync_f(&_dispatch_main_q,
//!   …)` from its own main thread — and prints a marker if it ever returns. The
//!   parent requires that marker never to appear. If that call can return, the
//!   branch in `run_on_main` is not load-bearing and the whole design rests on
//!   nothing.
//!
//!   **MEASURED CORRECTION TO THE BRIEF.** W10 called this a deadlock and
//!   expected a hang. On Darwin 25.5 it is not a hang: the child dies on
//!   **SIGTRAP** (signal 5, exit 133), because current libdispatch detects that
//!   the target queue is already owned by the calling thread and calls its
//!   crash handler. The stage accepts a hang OR a signal — the property is "the
//!   naive call does not return" — and reports which it saw. The practical
//!   consequence is that the missing branch would not be a rare freeze but an
//!   unconditional crash on the first main-thread call.
//! * **Stage 3** re-executes the binary in `real` mode, where the child calls
//!   `run_on_main` from exactly the same position on its own main thread, and
//!   requires it to EXIT 0 well inside the same watchdog.
//!
//! Same binary, same thread, same queue, same watchdog: one hangs and one does
//! not. That difference is the whole claim, and it is measured.
//!
//! * **Stage 4** isolates the CAUSE. `dispatch_sync_f` from the main thread to a
//!   private serial queue returns immediately. So stage 2's hang is not
//!   "dispatch_sync blocks", it is specifically the main queue re-entered from
//!   the thread it is bound to.
//!
//! # The rest of the contract
//!
//! * **Stage 1** — the direct path runs the closure on the main thread and
//!   returns its value.
//! * **Stage 5** — the DISPATCHED path, from a secondary thread, with this
//!   thread driving a run loop so the main queue is actually serviced. The
//!   closure must report that it ran on the main thread, and its value must
//!   come back.
//! * **Stage 6** — the pool invariant the module docs promise, on BOTH paths:
//!   anything autoreleased inside the closure is released before `run_on_main`
//!   returns. Measured with a weak reference, which reads nil exactly when the
//!   object has been deallocated, rather than with `-retainCount` on a pointer
//!   that may already be freed — the read that SIGSEGV'd in W9's
//!   counterexample.
//!
//!   **Only the DIRECT arm discriminates, and that is stated rather than
//!   glossed.** Plant-verified: with the pool removed from both paths, the
//!   direct arm fails and the dispatched arm still passes, because
//!   libdispatch's main-queue drain wraps every callback in a pool of its own.
//!   The dispatched arm is confirmatory. It is kept because the invariant is
//!   published for both paths and a reader deserves to see both asked, and it
//!   is labelled because a stage that cannot fail must never be counted as
//!   evidence that it did.
//! * **Stage 7** — a panic inside the closure is re-raised on the CALLING
//!   thread with its payload intact, and never unwinds through libdispatch's C
//!   frame.

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(run() as u8)
}

const PASS: i32 = 0;
const FAIL: i32 = 1;

/// The signal a current libdispatch raises when it CATCHES a main-queue
/// `dispatch_sync` re-entered from the main thread — see stage 2.
const SIGTRAP: i32 = 5;

/// The env var that turns this binary into one of its own child probes.
const MODE: &str = "ATERM_OBJC_DISPATCH_DRIVE_MODE";

/// How long the parent waits before declaring the naive child hung.
///
/// Generous on purpose: a FALSE "it deadlocked" would be the worst outcome this
/// file could produce, and the `real` child in stage 3 proves the same budget
/// is far more than the working path needs.
const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(not(target_os = "macos"))]
fn run() -> i32 {
    eprintln!("objc-dispatch-drive: NOT RUN — libdispatch's main queue is a macOS/Darwin subject.");
    2
}

#[cfg(target_os = "macos")]
fn run() -> i32 {
    match std::env::var(MODE).as_deref() {
        Ok("naive") => macos::child_naive(),
        Ok("real") => macos::child_real(),
        _ => macos::parent(),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use aterm_objc::{Id, Obj, Sel, WeakObj, class, msg, run_on_main, sel};

    use super::{FAIL, MODE, PASS, SIGTRAP, WATCHDOG};

    // -----------------------------------------------------------------------
    // The raw libdispatch surface, re-declared HERE.
    //
    // `aterm_objc::dispatch` keeps its externs private, which is correct: the
    // crate exports a capability, not a queue. This driver needs the NAIVE call
    // the capability exists to avoid, so it declares its own — and that
    // separation is itself part of the evidence, because it means stage 2 is
    // calling libdispatch directly rather than asking the module under test to
    // misbehave.
    // -----------------------------------------------------------------------

    #[repr(C)]
    struct DispatchObjectS {
        _opaque: [u8; 0],
    }

    // SAFETY: libdispatch is part of `libSystem` and is linked into every
    // process on this platform.
    unsafe extern "C" {
        static _dispatch_main_q: DispatchObjectS;
        fn dispatch_sync_f(
            queue: *const DispatchObjectS,
            context: *mut c_void,
            work: unsafe extern "C" fn(*mut c_void),
        );
        fn dispatch_queue_create(
            label: *const std::ffi::c_char,
            attr: *const c_void,
        ) -> *mut DispatchObjectS;
        fn dispatch_release(obj: *mut DispatchObjectS);
    }

    // SAFETY: CoreFoundation is a system framework, always present.
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
    }

    /// A `dispatch_function_t` that only records that it ran.
    ///
    /// # Safety
    /// `ctx` must address a live `bool`.
    unsafe extern "C" fn set_flag(ctx: *mut c_void) {
        // SAFETY: the caller pins `ctx` to a live `bool` and is blocked on the
        // synchronous dispatch for the whole of this call.
        unsafe { *ctx.cast::<bool>() = true };
    }

    /// Whether this thread is the process main thread, asked of Foundation
    /// directly rather than through the type under test.
    ///
    /// Deliberately independent of `aterm_objc::MainThread`: a stage that used
    /// the same witness the capability uses could not catch the capability
    /// being wrong about the thread.
    fn is_main_thread() -> bool {
        // SAFETY: `+[NSThread isMainThread]` is a side-effect-free class-method
        // `BOOL` query, and the cast is exactly `-(BOOL)(id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> aterm_objc::Bool = msg();
            f(class(c"NSThread").as_id(), sel!(isMainThread)).as_bool()
        }
    }

    /// A fresh `NSObject`, +1.
    fn new_object() -> Obj {
        // SAFETY: `+alloc` then `-init` on `NSObject` is the canonical +1
        // construction, and both prototypes are `-(id)(id, SEL)`.
        unsafe {
            let alloc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let init: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let raw = alloc(class(c"NSObject").as_id(), sel!(alloc));
            Obj::from_owned(init(raw, sel!(init))).expect("a fresh NSObject")
        }
    }

    // -----------------------------------------------------------------------
    // THE CHILD PROBES.
    // -----------------------------------------------------------------------

    /// THE NAIVE CALL — `dispatch_sync_f` to the main queue, FROM the main
    /// thread. This must never return.
    ///
    /// On Darwin 25.5 this does not hang: libdispatch notices that the calling
    /// thread already owns the target queue and traps, so the process dies on
    /// SIGTRAP with "BUG IN CLIENT OF LIBDISPATCH: dispatch_sync called on
    /// queue already owned by current thread" in its crash report. The
    /// diagnostic goes to the crash log rather than to stderr, which is why the
    /// parent classifies by SIGNAL and not by output.
    pub fn child_naive() -> i32 {
        assert!(
            is_main_thread(),
            "the naive probe must run on the main thread"
        );
        eprintln!("child(naive): about to dispatch_sync_f to the main queue from the main thread");
        let mut ran = false;
        // SAFETY: the queue is libdispatch's own main-queue symbol, `set_flag`
        // matches `dispatch_function_t`, and the context is a live local.
        //
        // This is the DEADLOCK, entered deliberately. The process is expected to
        // stop here for good; the parent reaps it.
        unsafe {
            dispatch_sync_f(
                &raw const _dispatch_main_q,
                (&raw mut ran).cast::<c_void>(),
                set_flag,
            );
        }
        // Only reachable if the main queue can be re-entered from the main
        // thread, which would falsify the premise of `run_on_main`'s branch.
        println!("NAIVE-RETURNED ran={ran}");
        PASS
    }

    /// THE REAL CALL — `run_on_main` from the same position. This must return.
    pub fn child_real() -> i32 {
        assert!(
            is_main_thread(),
            "the real probe must run on the main thread"
        );
        let got = run_on_main(|_mt| 0xC0FFEE_usize);
        println!("REAL-RETURNED got={got:#x}");
        if got == 0xC0FFEE { PASS } else { FAIL }
    }

    // -----------------------------------------------------------------------
    // THE PARENT.
    // -----------------------------------------------------------------------

    pub fn parent() -> i32 {
        let mut failures: Vec<String> = Vec::new();
        let mut check = |ok: bool, what: &str| {
            if ok {
                eprintln!("  ok   {what}");
            } else {
                eprintln!("  FAIL {what}");
                failures.push(what.to_owned());
            }
        };

        if !is_main_thread() {
            eprintln!("objc-dispatch-drive: NOT RUN — main() is not on the main thread.");
            return 2;
        }

        // -- STAGE 1: the direct path ------------------------------------
        eprintln!("stage 1: run_on_main on the main thread is a DIRECT call");
        let (value, ran_on_main) = run_on_main(|_mt| (0xA7E_usize, is_main_thread()));
        check(value == 0xA7E, "stage1: the closure's value came back");
        check(ran_on_main, "stage1: the closure ran on the main thread");

        // -- STAGE 2: the naive call HANGS -------------------------------
        eprintln!("stage 2: the naive dispatch_sync_f to the main queue must NOT RETURN");
        match probe("naive") {
            ProbeOutcome::Signalled(sig) => {
                // What actually happens on Darwin 25: libdispatch DETECTS the
                // re-entrancy and traps. See the note above `child_naive`.
                eprintln!("       child died on signal {sig} (SIGTRAP is 5)");
                check(
                    sig == SIGTRAP,
                    "stage2: the naive main-queue dispatch_sync_f trapped instead of returning",
                );
            }
            ProbeOutcome::StillRunning => {
                // The classic form, if a future libdispatch drops the check.
                eprintln!("       child was still running at the watchdog — the classic hang");
                check(
                    true,
                    "stage2: the naive main-queue dispatch_sync_f never returned",
                );
            }
            ProbeOutcome::Exited { code, stdout } => {
                eprintln!("       child exited with {code:?}, stdout={stdout:?}");
                check(
                    !stdout.contains("naive-returned"),
                    "stage2: the naive dispatch_sync_f RETURNED — the hazard premise is false \
                     and run_on_main's branch is not load-bearing",
                );
            }
            ProbeOutcome::Failed(e) => {
                eprintln!("       {e}");
                check(false, "stage2: the naive probe could not be launched");
            }
        }

        // -- STAGE 3: run_on_main from the same position RETURNS ----------
        eprintln!("stage 3: run_on_main from the same position must RETURN");
        match probe("real") {
            ProbeOutcome::Exited { code, stdout } => {
                check(code == Some(PASS), "stage3: the real probe exited 0");
                check(
                    stdout.contains("real-returned got=0xc0ffee"),
                    "stage3: the real probe returned the closure's value",
                );
            }
            ProbeOutcome::StillRunning => {
                check(
                    false,
                    "stage3: run_on_main HUNG on the main thread — it is dispatching",
                );
            }
            ProbeOutcome::Signalled(sig) => {
                eprintln!("       child died on signal {sig}");
                check(
                    false,
                    "stage3: run_on_main TRAPPED on the main thread — it is dispatching, not \
                     calling directly",
                );
            }
            ProbeOutcome::Failed(e) => {
                eprintln!("       {e}");
                check(false, "stage3: the real probe could not be launched");
            }
        }

        // -- STAGE 4: the cause is the MAIN queue, not dispatch_sync -------
        eprintln!("stage 4: dispatch_sync_f to a PRIVATE serial queue does not hang");
        let mut ran = false;
        // SAFETY: a NULL label and NULL attributes request an unlabelled serial
        // queue; `set_flag` matches `dispatch_function_t`; `ran` is a live
        // local and the dispatch is synchronous.
        unsafe {
            let q = dispatch_queue_create(std::ptr::null(), std::ptr::null());
            assert!(!q.is_null(), "dispatch_queue_create");
            dispatch_sync_f(q, (&raw mut ran).cast::<c_void>(), set_flag);
            dispatch_release(q);
        }
        check(
            ran,
            "stage4: a private serial queue ran the function and returned",
        );

        // -- STAGE 5: the DISPATCHED path, driven by a real run loop -------
        eprintln!("stage 5: run_on_main from a SECONDARY thread reaches the main thread");
        let (tx, rx) = mpsc::channel::<(usize, bool, bool)>();
        let worker = std::thread::spawn(move || {
            let off_main_before = !is_main_thread();
            let (v, on_main_inside) = run_on_main(|_mt| (0xB0B_usize, is_main_thread()));
            let _ = tx.send((v, on_main_inside, off_main_before));
        });

        // The main thread has to SERVICE its queue or the worker blocks for
        // ever; that is the whole reason this stage needs a run loop.
        let deadline = Instant::now() + WATCHDOG;
        let mut got = None;
        while Instant::now() < deadline {
            // SAFETY: the mode is CoreFoundation's own default-mode constant and
            // this is the main thread's run loop.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 0) };
            if let Ok(v) = rx.try_recv() {
                got = Some(v);
                break;
            }
        }
        let _ = worker.join();
        match got {
            Some((v, on_main_inside, off_main_before)) => {
                check(
                    off_main_before,
                    "stage5: the caller really was off the main thread",
                );
                check(v == 0xB0B, "stage5: the closure's value crossed back");
                check(on_main_inside, "stage5: the closure ran ON the main thread");
            }
            None => check(
                false,
                "stage5: run_on_main never completed from a secondary thread",
            ),
        }

        // -- STAGE 6: the pool invariant, on BOTH paths --------------------
        eprintln!("stage 6: anything autoreleased inside the closure dies before the return");
        check(
            pool_drained_direct(),
            "stage6: DIRECT path drained its pool before returning",
        );
        check(
            pool_drained_dispatched(),
            "stage6: DISPATCHED path drained its pool before returning",
        );

        // -- STAGE 7: a panic crosses as DATA, not as an unwind ------------
        eprintln!("stage 7: a panic in the closure re-raises on the calling thread");
        let prior = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = std::panic::catch_unwind(|| {
            run_on_main(|_mt| -> usize { panic!("w10-panic-payload") });
        });
        std::panic::set_hook(prior);
        match caught {
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                check(
                    msg == "w10-panic-payload",
                    "stage7: the original panic payload arrived at the call site",
                );
            }
            Ok(()) => check(false, "stage7: the panic was swallowed"),
        }

        if failures.is_empty() {
            eprintln!("objc-dispatch-drive: PASS");
            PASS
        } else {
            eprintln!("objc-dispatch-drive: FAIL ({} stage(s))", failures.len());
            for f in &failures {
                eprintln!("  - {f}");
            }
            FAIL
        }
    }

    /// The DIRECT path's pool check — no thread crossing, so the weak reference
    /// can simply be held across the call.
    fn pool_drained_direct() -> bool {
        let obj = new_object();
        let weak = WeakObj::from_obj(&obj);
        let addr = obj.id().expose_provenance();

        run_on_main(|_mt| {
            // A SECOND +1, handed straight to whatever pool is innermost. If
            // `run_on_main` pushed one, this dies at the return; if it did not,
            // it outlives the call.
            //
            // SAFETY: `addr` came from a live `Obj` that the caller still holds
            // for the whole of this call, so the address is a live object.
            let extra =
                unsafe { Obj::retain(Id::from_ptr(std::ptr::with_exposed_provenance_mut(addr))) }
                    .expect("the object is live");
            let _ = extra.autorelease();
        });

        // Drop OUR +1. If the closure's autoreleased +1 was already released —
        // i.e. the pool popped inside `run_on_main` — this was the last one and
        // the weak slot reads nil. If the pool had not popped, the object is
        // still alive here and the slot reads non-nil.
        drop(obj);
        weak.load().is_none()
    }

    /// The DISPATCHED path's pool check.
    ///
    /// Same differential as [`pool_drained_direct`], but the closure runs on the
    /// main thread while THIS thread is a worker, so the main thread has to be
    /// driving its run loop. That inverts the roles: the check runs on a worker
    /// and the main thread pumps.
    fn pool_drained_dispatched() -> bool {
        let (tx, rx) = mpsc::channel::<bool>();
        let worker = std::thread::spawn(move || {
            let obj = new_object();
            let weak = WeakObj::from_obj(&obj);
            let addr = obj.id().expose_provenance();
            run_on_main(move |_mt| {
                // SAFETY: the worker holds a +1 on this object across the whole
                // call, so the address is live.
                let extra = unsafe {
                    Obj::retain(Id::from_ptr(std::ptr::with_exposed_provenance_mut(addr)))
                }
                .expect("the object is live");
                let _ = extra.autorelease();
            });
            drop(obj);
            let _ = tx.send(weak.load().is_none());
        });

        let deadline = Instant::now() + WATCHDOG;
        let mut out = None;
        while Instant::now() < deadline {
            // SAFETY: as in stage 5.
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, 0) };
            if let Ok(v) = rx.try_recv() {
                out = Some(v);
                break;
            }
        }
        let _ = worker.join();
        out.unwrap_or(false)
    }

    // -----------------------------------------------------------------------
    // CHILD-PROCESS PLUMBING.
    // -----------------------------------------------------------------------

    enum ProbeOutcome {
        /// Still running when the watchdog fired — reaped by then. The CLASSIC
        /// deadlock: the queue never reaches the work and nobody notices.
        StillRunning,
        /// Killed by a signal. On a current libdispatch this is how the naive
        /// call ends: the re-entrancy is DETECTED and trapped, rather than
        /// hung. See stage 2.
        Signalled(i32),
        /// Returned to the OS under its own power.
        Exited {
            code: Option<i32>,
            stdout: String,
        },
        Failed(String),
    }

    /// Re-execute this binary in `mode` and watch it for [`WATCHDOG`].
    fn probe(mode: &str) -> ProbeOutcome {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return ProbeOutcome::Failed(format!("current_exe: {e}")),
        };
        let mut child = match std::process::Command::new(exe)
            .env(MODE, mode)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ProbeOutcome::Failed(format!("spawn: {e}")),
        };

        let deadline = Instant::now() + WATCHDOG;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    use std::os::unix::process::ExitStatusExt as _;
                    if let Some(sig) = status.signal() {
                        return ProbeOutcome::Signalled(sig);
                    }
                    let mut stdout = String::new();
                    if let Some(mut out) = child.stdout.take() {
                        use std::io::Read as _;
                        let _ = out.read_to_string(&mut stdout);
                    }
                    return ProbeOutcome::Exited {
                        code: status.code(),
                        stdout: stdout.to_lowercase(),
                    };
                }
                Ok(None) => {}
                Err(e) => return ProbeOutcome::Failed(format!("try_wait: {e}")),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return ProbeOutcome::StillRunning;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
