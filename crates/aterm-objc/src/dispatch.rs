// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synchronous main-queue execution ([`run_on_main`]) over libdispatch.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;

use crate::declare::MainThread;
use crate::runtime::autoreleasepool;

/// The opaque `dispatch_queue_t` the main queue's global symbol names, and every queue
/// [`crate::SerialQueue`] makes; only its address is used.
#[repr(C)]
pub(crate) struct DispatchObjectS {
    _opaque: [u8; 0],
}

// SAFETY: `_dispatch_main_q` and `dispatch_sync_f` are provided by libSystem.
unsafe extern "C" {
    /// The main queue (`&_dispatch_main_q`).
    static _dispatch_main_q: DispatchObjectS;

    /// `dispatch_sync_f(queue, context, work)` — the function-pointer form, which
    /// [`crate::SerialQueue::sync`] calls too (one declaration, one prototype).
    pub(crate) fn dispatch_sync_f(
        queue: *const DispatchObjectS,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

/// The slot the trampoline and the caller share for one call.
struct Job<F, R> {
    /// Closure taken by the trampoline.
    f: Option<F>,
    /// Result or caught panic payload filled by the trampoline.
    out: Option<std::thread::Result<R>>,
}

/// Panic payload raised on the calling thread when an `NSException` is contained inside [`run_on_main`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainedInRunOnMain {
    /// Containment report ordinal from [`crate::exception::Contained::ordinal`].
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

/// Main-queue C callback that catches Rust panics and `NSException`s into `Job::out`.
///
/// # Safety
/// `ctx` must be the address of a live `Job<F, R>` whose `f` is `Some`, and the
/// caller must be blocked on `dispatch_sync_f` for the whole call, which makes
/// the `&mut` below unique.
unsafe extern "C" fn trampoline<F, R>(ctx: *mut c_void)
where
    F: FnOnce(MainThread) -> R,
{
    // SAFETY: `ctx` points to a uniquely borrowed `Job<F, R>` while the caller blocks in `dispatch_sync_f`.
    let job = unsafe { &mut *ctx.cast::<Job<F, R>>() };
    let Some(f) = job.f.take() else {
        return;
    };

    let ran = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mt = MainThread::new()
            .expect("dispatch_sync_f on the main queue ran its function off the main thread");
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

/// Runs `f` on the process main thread inside an autorelease pool and returns its result.
///
/// # Blocking
/// Off the main thread this waits for the main queue, so two situations hang silently: a main
/// thread that never services its queue (call this only where the main thread runs an event
/// loop; a libtest binary's main thread is parked joining workers), and a main thread inside a
/// `run_on_main` closure that waits on a thread calling `run_on_main` (reachable from safe code,
/// e.g. a closure that spawns and joins a helper). Both are `dispatch_sync` semantics.
///
/// # Panics
///
/// Re-raises any panic from `f`, or panics with [`ContainedInRunOnMain`] if an
/// `NSException` was contained. Off the main thread, an `NSException` that `f`
/// raises is contained on the main thread (reported against `"run_on_main"`
/// through [`crate::exception`], Rust frames dropped, main thread continuing).
/// Also panics if libdispatch never runs the submitted function.
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
    // Hot: winit's `get_modifierless_char` reaches this on every ordinary
    // character key event, so `MainThread::new` caches its class lookup.
    if let Some(mt) = MainThread::new() {
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
        Some(Err(payload)) => std::panic::resume_unwind(payload),
        None => panic!("dispatch_sync_f returned without running the submitted function"),
    }
}
