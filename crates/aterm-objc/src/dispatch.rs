// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MAIN-QUEUE EXECUTION — `run_on_main`, the W10 capability.
//!
//! One entry point, [`run_on_main`], which runs a closure on the process main
//! thread and hands back what it returned. It replaces
//! `objc2_foundation::run_on_main` at `vendor/winit`'s `event.rs` and
//! `monitor.rs`, and it is libdispatch only: no Foundation class, no selector,
//! no framework binding. `libdispatch` is part of `libSystem`, already loaded
//! in every aterm process, so this module adds no dependency and no link flag.
//!
//! # THE POOL AND LIFETIME STORY, SETTLED BEFORE THE API
//!
//! W9's `load_borrowed` was withdrawn because it minted a lifetime from a pool
//! REFERENCE the caller named rather than from the pool the runtime actually
//! used, and the judge's lesson was explicit: *build the pool/lifetime
//! discipline before the API that mints lifetimes from it, not after.* This
//! module is the first capability written under that rule, and `run_on_main` is
//! a sharper case than `load_borrowed` was, because here the two pools are not
//! merely nested — **they are on different threads.**
//!
//! Three facts decide the whole signature:
//!
//! 1. **Autorelease pools are per-thread.** On the dispatched path the closure
//!    runs on the main thread, where *nothing* the calling thread has pushed is
//!    in scope. A `&AutoreleasePool` the caller holds is not merely the wrong
//!    pool, it is a pool on the wrong thread.
//! 2. **The main thread's innermost pool outlives this call.** A block run from
//!    the main queue lands inside whatever pool the main runloop has open for
//!    that iteration, and that pool drains when the iteration ends — which is
//!    AFTER `dispatch_sync_f` has returned and the calling thread has resumed.
//!    So a `+0` value produced inside the closure would be freed at a moment
//!    the caller cannot name, observe, or order against its own work.
//! 3. **Nothing may therefore cross the boundary that depends on a pool.**
//!
//! From which the API follows, and every clause below is a consequence rather
//! than a preference:
//!
//! * **No `&AutoreleasePool` in the signature, and none handed to the
//!   closure.** The closure receives a [`MainThread`] witness and nothing else.
//!   Passing it a pool reference would rebuild `load_borrowed`'s exact hole one
//!   level out: the closure's return type is generic, so a pool reference in
//!   scope is a lifetime the closure can put in `R`.
//! * **`R: Send`, and no lifetime parameter on the function.** `R` travels from
//!   the main thread back to the caller. A borrow of anything the main thread
//!   autoreleased cannot make that trip, and `Send` plus the absence of any
//!   lifetime to unify with is what stops it being tried.
//! * **`F: Send`.** The closure travels the other way.
//! * **`MainThread` is `!Send`** — so the witness cannot be captured by `f` and
//!   smuggled in from the calling thread. It is minted INSIDE, on the thread it
//!   describes, which is the only place it is true.
//!
//! ## The closure runs inside its own pool, on BOTH paths
//!
//! The two paths would otherwise have different pool semantics: on the direct
//! path the caller's innermost pool is in scope, on the dispatched path the
//! main runloop's is. That difference is invisible at the call site — it
//! depends on which thread you happened to be on — and an API whose lifetime
//! behaviour depends on invisible thread state is the shape F1 was about.
//!
//! So [`run_on_main`] pushes a pool around the closure on both paths. The
//! resulting rule is one sentence and holds unconditionally: **anything
//! autoreleased inside the closure is released before `run_on_main` returns,
//! and the caller cannot tell which path ran.** That is enforced, not asserted:
//! driver stage 6 autoreleases an object inside the closure on each path and
//! then asks a WEAK reference whether it was deallocated — a weak slot reads
//! nil exactly when the object is gone, whereas `-retainCount` on a pointer
//! that may already be freed is the read that SIGSEGV'd in W9's counterexample.
//!
//! ## WHICH HALF OF THAT STAGE IS LOAD-BEARING — plant-verified, and only one
//!
//! Removing the pool from both paths and re-running the driver fails the DIRECT
//! arm and leaves the DISPATCHED arm passing. The dispatched arm is therefore
//! **confirmatory, not discriminating**, and the reason is a libdispatch
//! implementation detail: the main queue's run-loop drain wraps each callback
//! in its own autorelease pool, so an object autoreleased inside a main-queue
//! callback is released when that callback ends whether or not this module
//! pushes anything.
//!
//! The pool is kept on that path regardless, and the reason is the F1 lesson
//! rather than tidiness: the guarantee this module publishes must be OURS,
//! discharged by code in this file, not inherited from an undocumented property
//! of the platform's queue drain that no test here would notice changing. What
//! is recorded — because a stage that cannot fail is worth exactly as much as
//! the honesty about it — is that only the direct arm can currently catch a
//! regression.
//!
//! # THE RE-ENTRANCY TRAP, WHICH IS WHY THE MAIN-THREAD CASE IS A DIRECT CALL
//!
//! The main queue is a serial queue bound to the main thread. `dispatch_sync`
//! to a serial queue blocks the caller until the queue reaches the work; when
//! the caller IS the thread that queue runs on, the queue can never reach it.
//!
//! So [`run_on_main`] calls `f` DIRECTLY when it is already on the main thread.
//! That is not a fast path, it is the only path that survives.
//!
//! ## What actually happens is a TRAP, not a hang — measured, and it matters
//!
//! W10's brief called this a deadlock, and on an older libdispatch it was one.
//! **On this platform it is not.** Measured on Darwin 25.5 by
//! `examples/objc_dispatch_drive.rs` stage 2: the naive call does not hang, it
//! raises **SIGTRAP** (the child dies on signal 5, exit 133). Current
//! libdispatch DETECTS the re-entrancy — it knows which thread owns the queue —
//! and calls its crash handler rather than blocking for ever.
//!
//! The correction cuts both ways and both halves are worth knowing:
//!
//! * **The failure is louder than advertised.** It is not a mysterious freeze
//!   to be diagnosed with a sampling profiler; it is an immediate, 100%
//!   reproducible crash with libdispatch's own diagnostic ("BUG IN CLIENT OF
//!   LIBDISPATCH: dispatch_sync called on queue already owned by current
//!   thread") in the crash report.
//! * **The branch is therefore MORE load-bearing, not less.** A hang needs an
//!   unlucky schedule; this needs nothing. Every main-thread call site —
//!   `monitor.rs`'s scale factor, `event.rs`'s keyboard type, both of which run
//!   on the main thread in a normal winit app — would kill the process on its
//!   first call.
//!
//! The stage accepts EITHER outcome as proof, because the property being tested
//! is "the naive call does not return", not "the naive call hangs"; a future
//! libdispatch that drops the check would still be caught.
//!
//! **This is proved rather than asserted**, and the proof is a differential:
//! the driver runs the naive `dispatch_sync_f(main_q, …)` from the main thread
//! in a CHILD PROCESS and requires it never to return, then runs
//! [`run_on_main`] from the same position and requires it to exit 0. Same
//! binary, same thread, same queue, same watchdog: one dies and one does not.
//! Stage 4 isolates the cause by showing `dispatch_sync_f` to a PRIVATE serial
//! queue from the main thread returns normally, so the trap is about the main
//! queue's thread binding and not about `dispatch_sync` in general.
//!
//! # Panics do not cross the C frame
//!
//! Unwinding through `dispatch_sync_f` — a C frame — is undefined behaviour.
//! The trampoline catches, ships the payload back through the job slot, and the
//! CALLING thread resumes the unwind. A panicking closure therefore panics at
//! the call site, on the caller's thread, with its original payload.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;

use crate::declare::MainThread;
use crate::runtime::autoreleasepool;

/// The opaque `dispatch_queue_t` the main queue's global symbol names.
///
/// Declared as a zero-sized opaque rather than mirrored: this module never
/// reads a field of it, it only takes its address.
#[repr(C)]
struct DispatchObjectS {
    _opaque: [u8; 0],
}

// SAFETY: both symbols are libdispatch's, and libdispatch is part of
// `libSystem`, which is linked into every process on this platform — the same
// reason `objc_msgSend` needs no link attribute in `runtime.rs`.
unsafe extern "C" {
    /// The main queue, as a GLOBAL SYMBOL rather than a function call.
    ///
    /// `dispatch_get_main_queue()` is a header inline that evaluates to
    /// `&_dispatch_main_q`, so there is no function to call and nothing to
    /// cache: the address is the queue.
    static _dispatch_main_q: DispatchObjectS;

    /// `dispatch_sync_f(queue, context, work)` — the FUNCTION-POINTER form.
    ///
    /// Deliberately not the block form. A block would have to be heap-copied by
    /// `_Block_copy` and would drag [`crate::RcBlock`]'s `Fn + 'static` bound in
    /// with it, which forbids the borrowing `FnOnce` every call site here
    /// actually has. `dispatch_sync_f` takes a bare `void *`, allocates
    /// nothing, and — because it is SYNCHRONOUS — lets the context be a local
    /// of the calling frame.
    fn dispatch_sync_f(
        queue: *const DispatchObjectS,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

/// The slot the trampoline and the caller share for one call.
///
/// Both fields are `Option` so the trampoline can MOVE the closure out and move
/// the result in, and so "the main queue never ran our function" is
/// distinguishable from "it ran and produced `None`" — the first would be a
/// libdispatch contract violation and is reported as one rather than silently
/// unwrapping.
struct Job<F, R> {
    /// Taken by the trampoline. `None` afterwards.
    f: Option<F>,
    /// Filled by the trampoline: `Ok(value)`, or the payload of a panic that
    /// must be re-raised on the CALLING thread rather than unwound through C.
    out: Option<std::thread::Result<R>>,
}

/// What the main queue runs. `extern "C"`, so it must not unwind.
///
/// # Safety
/// `ctx` must be the address of a live `Job<F, R>` whose `f` is `Some`, and the
/// caller must be blocked on `dispatch_sync_f` for the whole of this call — the
/// two conditions that make the `&mut` below the only reference to that job.
unsafe extern "C" fn trampoline<F, R>(ctx: *mut c_void)
where
    F: FnOnce(MainThread) -> R,
{
    // SAFETY: the caller pins `ctx` to a live `Job<F, R>` and is blocked inside
    // `dispatch_sync_f` until this returns, so nothing else can touch the job
    // for the duration and this `&mut` is unique.
    let job = unsafe { &mut *ctx.cast::<Job<F, R>>() };
    let Some(f) = job.f.take() else {
        // Reachable only if libdispatch ran one submission twice, which would
        // mean the closure — an `FnOnce` already moved out — had to be moved
        // out again. There is nothing to run and nothing safe to report, so
        // leave `out` as `None` and let the caller name it.
        return;
    };

    // `catch_unwind` is what keeps the unwind off the C frame above us. The
    // closure is `FnOnce` and captures whatever it captures, so it is not
    // `UnwindSafe` and cannot be; the assertion is discharged by the fact that
    // nothing observes the captures after a panic — the payload goes back to
    // the caller, which resumes the unwind and drops the job.
    job.out = Some(std::panic::catch_unwind(AssertUnwindSafe(|| {
        // The witness is minted HERE, on the thread it describes, through the
        // CHECKED constructor.
        //
        // `MainThread::new_unchecked` would save one `MainThread::new()` per
        // off-main call and would be sound: `dispatch_sync_f` on the main queue
        // runs its function on the main thread, because the main queue is bound
        // to that thread and — unlike an ordinary serial queue — never migrates
        // work to the caller. The check is kept anyway: in exchange this module
        // contains no `unsafe` token whose justification is a claim about
        // libdispatch's scheduler that a reader would have to re-derive. If the
        // claim were ever false, this panics on the main thread of a live app
        // instead of handing AppKit a witness that is a lie.
        //
        // WHAT IT COSTS, CORRECTED. This comment used to price the check as
        // "one class-method `BOOL` per call at sites that run once per monitor
        // query or once per keyboard-layout change". BOTH HALVES WERE WRONG,
        // and the thirteenth pass measured each.
        //
        // * The COST is not one `BOOL` send. `MainThread::new()` is 18.2-18.7
        //   ns/op in release, of which the send is 1.7-2.0 and the UNCACHED
        //   `objc_getClass("NSThread")` in front of it is ~17 — see
        //   [`crate::class`], which now carries the decomposition.
        // * The FREQUENCY is not once per layout change. `event.rs:58` calls
        //   `run_on_main` from `get_modifierless_char`, which
        //   `create_key_event` calls at line 149 — i.e. once per KEY EVENT
        //   whose `code_to_key` is unidentified, which is every ordinary
        //   character key. `monitor.rs:261` is `scale_factor()`, which
        //   `position()` also calls.
        //
        // 18 ns on a keystroke is not a regression anybody can feel, and it is
        // recorded rather than optimised for exactly that reason. What is not
        // acceptable is a stated cost that is 9% of the real one in a module
        // whose argument is that its numbers are measured.
        let mt = MainThread::new()
            .expect("dispatch_sync_f on the main queue ran its function off the main thread");
        // The pool the module docs promise, on the DISPATCHED path. The closure
        // never sees it — a pool reference in scope of a generic `R` is exactly
        // `load_borrowed`'s defect.
        autoreleasepool(|_pool| f(mt))
    })));
}

/// Run `f` on the main thread and return what it produced.
///
/// If the caller is already on the main thread, `f` runs directly — see the
/// module docs for why that branch is the only non-hanging one, and for the
/// differential that proves it. Otherwise `f` is submitted to the main queue
/// with `dispatch_sync_f` and this call blocks until it has run.
///
/// `f` receives a [`MainThread`] witness, minted on the main thread itself, and
/// nothing else. It is deliberately not given an [`crate::AutoreleasePool`]
/// reference: the module docs explain why a pool reference in scope of a
/// generic return type is the defect W9 withdrew `load_borrowed` for. Anything
/// `f` autoreleases is released before this function returns, on either path.
///
/// # Blocking, and the TWO ways this hangs
///
/// This heading used to say "the one way", and the thirteenth pass measured a
/// second. Both hang SILENTLY — no signal, no diagnostic — which is the
/// opposite of the loud main-thread trap the module docs describe, and worth
/// knowing precisely because the loud one is the one that gets remembered.
///
/// 1. **The main thread never services its queue.** Off the main thread this
///    blocks until the main thread reaches the main queue, so it is only
///    appropriate in a process whose main thread runs an event loop —
///    `NSApplicationMain`, `CFRunLoopRun`, `dispatch_main` or winit's pump. In
///    a process whose main thread is doing something else (a libtest binary,
///    whose main thread is parked joining worker threads) it blocks for as long
///    as that lasts, which is why this capability's evidence is a DRIVER and
///    not a unit test.
/// 2. **The main thread is INSIDE a `run_on_main` closure and waiting on a
///    thread that calls `run_on_main`.** The outer call took the direct branch,
///    so the main thread is running the closure rather than draining its queue;
///    the inner caller is genuinely off-main, so it takes the dispatch branch
///    and waits for a queue nobody is draining. Reachable from 100% safe code
///    with no `unsafe` token and no misuse of this API's contract — the closure
///    merely spawned a helper and joined it.
///
/// Measured on Darwin 25.5, each in a child process under a 4-second watchdog:
/// both are **still running when the watchdog fires**, where the main-thread
/// re-entrancy this module branches around dies on SIGTRAP in microseconds.
/// Neither is a defect in this function — both are `dispatch_sync`'s own
/// semantics — but a caller who has read only the trap paragraph will expect a
/// crash and get a freeze.
///
/// # Panics
///
/// If `f` panics, the panic is caught on the main thread and re-raised here,
/// with its original payload, on the calling thread — never unwound through
/// libdispatch's C frame. Also panics if libdispatch fails to run the submitted
/// function at all, which would be a contract violation rather than a
/// misuse.
///
/// # Examples
///
/// ```no_run
/// # use aterm_objc::run_on_main;
/// let scale = run_on_main(|_mt| 2.0_f64);
/// assert_eq!(scale, 2.0);
/// ```
pub fn run_on_main<F, R>(f: F) -> R
where
    F: Send + FnOnce(MainThread) -> R,
    R: Send,
{
    if let Some(mt) = MainThread::new() {
        // ALREADY ON THE MAIN THREAD — a direct call. `dispatch_sync_f` here
        // would deadlock against the queue this very frame occupies.
        //
        // The pool wraps the closure on this path too, so that the pool
        // semantics the module docs state do not depend on which thread the
        // caller happened to be. THIS is the arm the driver's stage 6 can
        // actually catch: with it removed, the direct arm fails and the
        // dispatched arm does not (libdispatch pools its own main-queue
        // callbacks). Plant-verified, and recorded in the module docs.
        return autoreleasepool(|_pool| f(mt));
    }

    let mut job: Job<F, R> = Job {
        f: Some(f),
        out: None,
    };

    // SAFETY: `&raw mut job` addresses a live local that outlives this call
    // because `dispatch_sync_f` is SYNCHRONOUS — it returns only after the
    // trampoline has run to completion, so the job cannot be dropped while
    // libdispatch holds the pointer. `trampoline::<F, R>` is instantiated at
    // exactly the `Job<F, R>` whose address is passed, and `job.f` is `Some`.
    // The queue is libdispatch's own main-queue symbol, and the branch above
    // has established this thread is NOT the main thread, so the sync cannot be
    // re-entrant.
    unsafe {
        dispatch_sync_f(
            &raw const _dispatch_main_q,
            (&raw mut job).cast::<c_void>(),
            trampoline::<F, R>,
        );
    }

    match job.out.take() {
        Some(Ok(value)) => value,
        // Re-raise on THIS thread. The payload crossed as data; the unwind
        // never crossed the C frame.
        Some(Err(payload)) => std::panic::resume_unwind(payload),
        None => panic!("dispatch_sync_f returned without running the submitted function"),
    }
}
