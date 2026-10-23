// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synchronous main-queue execution ([`run_on_main`]) over libdispatch.

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

// SAFETY: `_dispatch_main_q` and `dispatch_sync_f` are provided by libSystem.
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
    // SAFETY: `ctx` points to a uniquely borrowed `Job<F, R>` while the caller blocks in `dispatch_sync_f`.
    let job = unsafe { &mut *ctx.cast::<Job<F, R>>() };
    let Some(f) = job.f.take() else {
        // and let the caller report it.
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
/// # Panics
/// Re-raises any panic from `f`, or panics with [`ContainedInRunOnMain`] if an `NSException` was contained.
pub fn run_on_main<F, R>(f: F) -> R
where
    F: Send + FnOnce(MainThread) -> R,
    R: Send,
{
    // character key event, so `MainThread::new` caches its class lookup.
    if let Some(mt) = MainThread::new() {
        // pool semantics do not depend on the calling thread.
        return autoreleasepool(|_pool| f(mt));
    }

    let mut job: Job<F, R> = Job {
        f: Some(f),
        out: None,
    };

    // SAFETY: `job` outlives `dispatch_sync_f` and this thread is not the main thread.
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
