// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Main-queue execution: [`run_on_main`] runs a closure on the process main
//! thread and returns its result. libdispatch only (part of `libSystem`), so no
//! dependency or link flag.
//!
//! # Pools and lifetimes
//!
//! Autorelease pools are per-thread, and on the dispatched path the closure
//! runs on the main thread, where nothing the caller pushed is in scope; the
//! main run loop's pool drains after `dispatch_sync_f` has returned. So nothing
//! that depends on a pool may cross the boundary:
//!
//! * No `&AutoreleasePool` in the signature or handed to the closure (a pool
//!   reference in scope of a generic `R` would let it mint a lifetime). The
//!   closure receives only a [`MainThread`] witness, minted on the main thread
//!   (it is `!Send`, so it cannot be smuggled in).
//! * `R: Send` with no lifetime parameter; `F: Send`.
//!
//! The closure runs inside its own pool on both paths, so anything it
//! autoreleases is released before `run_on_main` returns and the caller cannot
//! tell which path ran. On the dispatched path the main queue's drain already
//! wraps each callback in a pool; the explicit one keeps the guarantee from
//! resting on that undocumented detail.
//!
//! # Re-entrancy: the main-thread case is a direct call
//!
//! The main queue is serial and bound to the main thread, so
//! `dispatch_sync_f(main_q, …)` from the main thread can never run. Current
//! libdispatch detects this and traps (SIGTRAP, "dispatch_sync called on queue
//! already owned by current thread"); `dispatch_sync_f` to a private serial
//! queue from the main thread is fine. [`run_on_main`] therefore calls `f`
//! directly on the main thread, which every winit call site hits.
//! `tests/main_thread_bound_drive.rs` (`harness = false`) drives both the
//! direct path (dropping a [`crate::MainThreadBound`] on the main thread) and
//! the dispatched path from workers.
//!
//! # Panics do not cross the C frame
//!
//! Unwinding through `dispatch_sync_f` is undefined behaviour. The trampoline
//! catches, ships the payload back through the job slot, and the calling
//! thread resumes the unwind with the original payload.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;

use crate::declare::MainThread;
use crate::runtime::autoreleasepool;

/// The opaque `dispatch_queue_t` the main queue's global symbol names; only its
/// address is used.
#[repr(C)]
struct DispatchObjectS {
    _opaque: [u8; 0],
}

// SAFETY: both symbols are libdispatch's, part of `libSystem`, which every
// process links.
unsafe extern "C" {
    /// The main queue. `dispatch_get_main_queue()` is a header inline that
    /// evaluates to `&_dispatch_main_q`, so the address is the queue.
    static _dispatch_main_q: DispatchObjectS;

    /// `dispatch_sync_f(queue, context, work)` — the function-pointer form. A
    /// block would need `_Block_copy` and [`crate::RcBlock`]'s `Fn + 'static`
    /// bound; this allocates nothing and, being synchronous, lets the context
    /// be a local of the calling frame.
    fn dispatch_sync_f(
        queue: *const DispatchObjectS,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

/// The slot the trampoline and the caller share for one call. Both fields are
/// `Option` so the trampoline can move the closure out and the result in, and
/// so "never ran" is distinguishable from "ran".
struct Job<F, R> {
    /// Taken by the trampoline. `None` afterwards.
    f: Option<F>,
    /// Filled by the trampoline: `Ok(value)`, or the payload of a panic that
    /// must be re-raised on the calling thread rather than unwound through C.
    out: Option<std::thread::Result<R>>,
}

/// The payload [`run_on_main`] re-raises on the calling thread when an
/// `NSException` was contained inside the closure on the main thread.
///
/// The main thread continues (the exception was caught, reported through
/// [`crate::exception`] against `"run_on_main"`, and released); the caller
/// gets a `resume_unwind` with this payload, which does not invoke the panic
/// hook. To survive it, wrap the call in `catch_unwind` and downcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainedInRunOnMain {
    /// This containment's report ordinal, carried out of the catch by
    /// [`crate::exception::Contained::ordinal`] (re-reading
    /// [`crate::contained_count`] could name another thread's report).
    pub ordinal: u64,
}

impl std::fmt::Display for ContainedInRunOnMain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "aterm-objc: an NSException raised inside a run_on_main closure was \
             contained on the main thread (containment #{}); see the containment log",
            self.ordinal
        )
    }
}

/// What the main queue runs. `extern "C"`, so it must not unwind: a Rust panic
/// is caught and carried back as data, and an `NSException` is contained and
/// carried back the same way.
///
/// # Safety
/// `ctx` must be the address of a live `Job<F, R>` whose `f` is `Some`, and the
/// caller must be blocked on `dispatch_sync_f` for the whole call, which makes
/// the `&mut` below unique.
unsafe extern "C" fn trampoline<F, R>(ctx: *mut c_void)
where
    F: FnOnce(MainThread) -> R,
{
    // SAFETY: `ctx` is a live `Job<F, R>` and the caller is blocked in
    // `dispatch_sync_f` until this returns, so this `&mut` is unique.
    let job = unsafe { &mut *ctx.cast::<Job<F, R>>() };
    let Some(f) = job.f.take() else {
        // Only if libdispatch ran one submission twice; leave `out` as `None`
        // and let the caller report it.
        return;
    };

    // Keeps the unwind off the C frame above. `AssertUnwindSafe` is discharged
    // because nothing observes the captures after a panic: the payload goes
    // back to the caller, which resumes the unwind and drops the job.
    let ran = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // The witness is minted here through the checked constructor. The
        // unchecked one would be sound (the main queue always runs on the main
        // thread), but the check avoids an `unsafe` resting on a claim about
        // libdispatch's scheduler.
        let mt = MainThread::new()
            .expect("dispatch_sync_f on the main queue ran its function off the main thread");
        // The pool from the module docs, on the dispatched path; the closure
        // never sees it. Containment sits inside the pool and the
        // `catch_unwind`, so an `NSException` out of `f` is caught on the main
        // thread, reported, and released before the pool pops.
        autoreleasepool(|_pool| crate::exception::contain("run_on_main", || f(mt)))
    }));
    job.out = Some(match ran {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(contained)) => Err(Box::new(ContainedInRunOnMain {
            ordinal: contained.ordinal(),
        })),
        Err(payload) => Err(payload),
    });
}

/// Run `f` on the main thread and return what it produced.
///
/// On the main thread `f` runs directly (see the module docs); otherwise it is
/// submitted with `dispatch_sync_f` and this call blocks until it has run. `f`
/// receives a [`MainThread`] witness and nothing else; anything it autoreleases
/// is released before this returns, on either path.
///
/// # Blocking
///
/// Two situations hang silently (unlike the loud main-thread trap):
///
/// 1. The main thread never services its queue. Only use this in a process
///    whose main thread runs an event loop (`NSApplicationMain`,
///    `CFRunLoopRun`, `dispatch_main`, winit's pump); a libtest binary's main
///    thread is parked joining workers.
/// 2. The main thread is inside a `run_on_main` closure (direct branch) and
///    waits on a thread that calls `run_on_main`: that call dispatches to a
///    queue nobody is draining. Reachable from safe code, e.g. a closure that
///    spawns and joins a helper.
///
/// Both are `dispatch_sync` semantics, not defects in this function.
///
/// # Panics
///
/// If `f` panics, the panic is caught on the main thread and re-raised here
/// with its original payload. If `f` raises an `NSException`, it is contained
/// on the main thread (reported against `"run_on_main"` through
/// [`crate::exception`], Rust frames dropped, main thread continuing) and
/// re-raised here as a panic with payload [`ContainedInRunOnMain`]. Also
/// panics if libdispatch never runs the submitted function.
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
    // `MainThread::new` costs about 18 ns (mostly the uncached `objc_getClass`,
    // see [`crate::class`]), paid on every ordinary character key event via
    // winit's `get_modifierless_char`.
    if let Some(mt) = MainThread::new() {
        // Already on the main thread: call directly, since `dispatch_sync_f`
        // here would trap. The pool wraps the closure on this path too, so the
        // pool semantics do not depend on the calling thread.
        return autoreleasepool(|_pool| f(mt));
    }

    let mut job: Job<F, R> = Job {
        f: Some(f),
        out: None,
    };

    // SAFETY: `job` is a live local that outlives the call because
    // `dispatch_sync_f` returns only after the trampoline has finished.
    // `trampoline::<F, R>` matches the `Job<F, R>` passed, and `job.f` is
    // `Some`. The branch above established this is not the main thread, so the
    // sync is not re-entrant.
    unsafe {
        dispatch_sync_f(
            &raw const _dispatch_main_q,
            (&raw mut job).cast::<c_void>(),
            trampoline::<F, R>,
        );
    }

    match job.out.take() {
        Some(Ok(value)) => value,
        // Re-raise on this thread. The payload crossed as data; the unwind
        // never crossed the C frame.
        Some(Err(payload)) => std::panic::resume_unwind(payload),
        None => panic!("dispatch_sync_f returned without running the submitted function"),
    }
}
