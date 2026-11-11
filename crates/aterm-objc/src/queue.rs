// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private serial dispatch queue ([`SerialQueue`]) over libdispatch, beside
//! [`crate::run_on_main`]'s main queue: the queue a framework object that is made on one and
//! messaged only on it (Virtualization.framework's `VZVirtualMachine`) lives on, so the
//! process's main thread never needs a run loop.
//!
//! Everything here is libSystem's (`dispatch_queue_create`, `dispatch_sync_f`,
//! `dispatch_async_f`, `dispatch_queue_set_specific`, `dispatch_get_specific`,
//! `dispatch_release`), so the crate keeps its zero dependencies.

use std::ffi::{CStr, c_char, c_void};
use std::panic::AssertUnwindSafe;

use crate::runtime::{Id, autoreleasepool};

/// The opaque `dispatch_queue_t` libdispatch hands out, shared with [`crate::run_on_main`]'s
/// declarations so `dispatch_sync_f` has one prototype; only its address is used.
type DispatchQueueS = crate::dispatch::DispatchObjectS;

// SAFETY: these are libSystem's, with the C prototypes `<dispatch/queue.h>` and
// `<dispatch/object.h>` give them (`dispatch_queue_t` and `dispatch_object_t` are pointers;
// `DISPATCH_QUEUE_SERIAL` is null).
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut DispatchQueueS;
    fn dispatch_async_f(
        queue: *mut DispatchQueueS,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_queue_set_specific(
        queue: *mut DispatchQueueS,
        key: *const c_void,
        context: *mut c_void,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    );
    fn dispatch_get_specific(key: *const c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

/// The queue-specific key every [`SerialQueue`] marks itself under, with its own address
/// as the value: only its address is used.
static CURRENT: u8 = 0;

/// Panic payload raised on the calling thread when an `NSException` is contained inside
/// [`SerialQueue::sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainedOnQueue {
    /// Containment report ordinal from [`crate::exception::Contained::ordinal`].
    pub ordinal: u64,
}

impl std::fmt::Display for ContainedOnQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "aterm-objc: an NSException raised inside a SerialQueue::sync closure was \
             contained on the queue (containment #{}); see the containment log",
            self.ordinal
        )
    }
}

/// A private serial dispatch queue: work runs one item at a time, in the order it was
/// submitted, on a libdispatch worker thread (or, for [`Self::sync`], possibly the caller's
/// own, which libdispatch may borrow while the caller waits).
pub struct SerialQueue {
    raw: *mut DispatchQueueS,
}

// SAFETY: a dispatch queue is a thread-safe, reference-counted libdispatch object: any
// thread may submit to it or release the reference this holder owns.
unsafe impl Send for SerialQueue {}
// SAFETY: as above; `&SerialQueue` only submits work and reads the queue-specific mark.
unsafe impl Sync for SerialQueue {}

impl std::fmt::Debug for SerialQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SerialQueue({:p})", self.raw)
    }
}

/// The slot the trampoline and the caller share for one [`SerialQueue::sync`].
struct Job<F, R> {
    /// The closure, taken by the trampoline.
    f: Option<F>,
    /// Its result, or the caught panic payload, filled by the trampoline.
    out: Option<std::thread::Result<R>>,
}

/// The queue's C callback for [`SerialQueue::sync`]: runs the closure in an autorelease
/// pool, catching Rust panics and containing `NSException`s into `Job::out`.
///
/// # Safety
/// `ctx` must be the address of a live `Job<F, R>`, and the caller must be blocked in
/// `dispatch_sync_f` for the whole call, which makes the `&mut` below unique.
unsafe extern "C" fn sync_trampoline<F, R>(ctx: *mut c_void)
where
    F: FnOnce() -> R,
{
    // SAFETY: `ctx` is the uniquely borrowed `Job<F, R>` of a caller blocked in `dispatch_sync_f`.
    let job = unsafe { &mut *ctx.cast::<Job<F, R>>() };
    let Some(f) = job.f.take() else {
        return;
    };
    let ran = std::panic::catch_unwind(AssertUnwindSafe(|| {
        autoreleasepool(|_pool| crate::exception::contain("SerialQueue::sync", f))
    }));
    job.out = Some(match ran {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(contained)) => Err(Box::new(ContainedOnQueue {
            ordinal: contained.ordinal(),
        })),
        Err(payload) => Err(payload),
    });
}

/// The queue's C callback for [`SerialQueue::spawn`]: takes the boxed closure back and runs
/// it in an autorelease pool. A panic or a contained `NSException` is reported on stderr
/// and the queue goes on: nothing waits to receive it, and no Objective-C frame is unwound.
///
/// # Safety
/// `ctx` must be a `Box<F>` that [`SerialQueue::spawn`] leaked for exactly this call.
unsafe extern "C" fn async_trampoline<F>(ctx: *mut c_void)
where
    F: FnOnce(),
{
    // SAFETY: `spawn` leaked exactly this `Box<F>` for this one call.
    let f = unsafe { Box::from_raw(ctx.cast::<F>()) };
    let ran = std::panic::catch_unwind(AssertUnwindSafe(|| {
        autoreleasepool(|_pool| crate::exception::contain("SerialQueue::spawn", *f))
    }));
    if !matches!(ran, Ok(Ok(()))) {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr().lock(),
            "aterm-objc: SerialQueue::spawn work ended in a panic or an NSException; the \
             queue goes on"
        );
    }
}

impl SerialQueue {
    /// A new serial queue named `label` (the name a crash report and `sample` show), or
    /// `None` if libdispatch made none.
    #[must_use]
    pub fn new(label: &CStr) -> Option<Self> {
        // SAFETY: a NUL-terminated label (libdispatch copies it) and the null attribute,
        // `DISPATCH_QUEUE_SERIAL`; the result is a +1 queue or null.
        let raw = unsafe { dispatch_queue_create(label.as_ptr(), std::ptr::null()) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a live queue; the key is a static's address and the value is the
        // queue's own address, read back only to compare, so no destructor is needed.
        unsafe {
            dispatch_queue_set_specific(raw, (&raw const CURRENT).cast(), raw.cast(), None);
        }
        Some(Self { raw })
    }

    /// The queue as the object a framework takes it as (`dispatch_queue_t` is an
    /// Objective-C object wherever the runtime is loaded, encoded `@`): borrowed for as long
    /// as `self` lives.
    #[must_use]
    pub fn as_id(&self) -> Id {
        Id::from_ptr(self.raw.cast())
    }

    /// Whether the calling code is running on this queue.
    #[must_use]
    pub fn is_current(&self) -> bool {
        // SAFETY: reads the current queue's value for a static key; never dereferenced.
        let mark = unsafe { dispatch_get_specific((&raw const CURRENT).cast()) };
        mark == self.raw.cast()
    }

    /// Runs `f` on this queue, after everything submitted before it, inside an
    /// autorelease pool, and returns its result.
    ///
    /// # Panics
    /// Called from work already on this queue (a `dispatch_sync` onto the current serial
    /// queue deadlocks, so it is refused instead). Re-raises any panic from `f`, or panics
    /// with [`ContainedOnQueue`] if `f` raised an `NSException`, which is contained on the
    /// queue and reported through [`crate::exception`].
    pub fn sync<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        assert!(
            !self.is_current(),
            "aterm-objc: SerialQueue::sync from work on the same queue would deadlock"
        );
        let mut job: Job<F, R> = Job {
            f: Some(f),
            out: None,
        };
        // SAFETY: `job` is a live local that outlives the call, because `dispatch_sync_f`
        // returns only after the trampoline has finished; `sync_trampoline::<F, R>` matches
        // the `Job<F, R>` passed, and the call is not re-entrant on this queue (checked).
        unsafe {
            crate::dispatch::dispatch_sync_f(
                self.raw,
                (&raw mut job).cast::<c_void>(),
                sync_trampoline::<F, R>,
            );
        }
        match job.out.take() {
            Some(Ok(value)) => value,
            Some(Err(payload)) => std::panic::resume_unwind(payload),
            None => panic!("dispatch_sync_f returned without running the submitted function"),
        }
    }

    /// Submits `f` to run on this queue after everything submitted before it, and returns
    /// at once. The queue keeps itself alive until the work has run, even when this holder
    /// is dropped first.
    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let ctx = Box::into_raw(Box::new(f)).cast::<c_void>();
        // SAFETY: `ctx` is a leaked `Box<F>` that `async_trampoline::<F>` takes back exactly
        // once; libdispatch retains the queue until the work has run.
        unsafe { dispatch_async_f(self.raw, ctx, async_trampoline::<F>) };
    }
}

impl Drop for SerialQueue {
    fn drop(&mut self) {
        // SAFETY: this holder owns the one +1 reference `dispatch_queue_create` returned,
        // released exactly once; pending work holds its own references.
        unsafe { dispatch_release(self.raw.cast()) };
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
