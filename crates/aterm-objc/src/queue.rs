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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use super::*;

    /// The hang detector: a wait for what must happen fails after a minute, never sooner.
    const HANG: Duration = Duration::from_secs(60);

    fn queue() -> SerialQueue {
        SerialQueue::new(c"aterm-objc test queue").expect("a queue")
    }

    #[test]
    fn work_runs_one_at_a_time_in_submission_order_and_sync_waits_for_it() {
        let q = queue();
        let order = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicUsize::new(0));
        for i in 0..200 {
            let order = Arc::clone(&order);
            let running = Arc::clone(&running);
            q.spawn(move || {
                assert_eq!(
                    running.fetch_add(1, Ordering::SeqCst),
                    0,
                    "overlapping work"
                );
                order.lock().expect("order").push(i);
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
        let seen = q.sync(|| order.lock().expect("order").clone());
        assert_eq!(seen, (0..200).collect::<Vec<_>>());
        assert_eq!(q.sync(|| 7_u32), 7);
    }

    #[test]
    fn the_queue_knows_its_own_work_and_no_other() {
        let q = queue();
        let other = queue();
        assert!(!q.is_current());
        assert!(q.sync(|| q.is_current()));
        assert!(!q.sync(|| other.is_current()));
        let (tx, rx) = mpsc::channel();
        let q = Arc::new(q);
        let inner = Arc::clone(&q);
        q.spawn(move || {
            let _ = tx.send(inner.is_current());
        });
        assert_eq!(rx.recv_timeout(HANG), Ok(true));
    }

    #[test]
    fn a_sync_from_the_queues_own_work_is_refused_not_deadlocked() {
        let q = queue();
        let refused = std::panic::catch_unwind(AssertUnwindSafe(|| q.sync(|| q.sync(|| 1))));
        let payload = refused.expect_err("refused");
        let text = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert!(text.contains("would deadlock"), "{text}");
        // The queue is still usable.
        assert_eq!(q.sync(|| 2), 2);
    }

    #[test]
    fn a_panic_in_sync_reaches_the_caller_and_the_queue_goes_on() {
        let q = queue();
        let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
            q.sync(|| -> u8 { panic!("on the queue") })
        }));
        assert!(caught.is_err());
        assert_eq!(q.sync(|| 3), 3);
    }

    #[test]
    fn a_panic_in_spawned_work_is_reported_and_later_work_still_runs() {
        let q = queue();
        q.spawn(|| panic!("spawned work panics"));
        let (tx, rx) = mpsc::channel();
        q.spawn(move || {
            let _ = tx.send(5);
        });
        assert_eq!(rx.recv_timeout(HANG), Ok(5));
    }

    #[test]
    fn pending_work_outlives_the_dropped_holder() {
        let q = queue();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        q.spawn(move || {
            let _ = gate_rx.recv_timeout(HANG);
        });
        q.spawn(move || {
            let _ = done_tx.send(());
        });
        drop(q);
        gate_tx.send(()).expect("gate");
        assert_eq!(done_rx.recv_timeout(HANG), Ok(()));
    }

    #[test]
    fn the_queue_is_an_object_the_runtime_can_message() {
        let q = queue();
        let id = q.as_id();
        assert!(!id.is_null());
        // SAFETY: a dispatch queue is an Objective-C object here; `-description` is `@16@0:8`
        // and answers an autoreleased NSString, read inside the pool.
        let text = autoreleasepool(|_| unsafe {
            let description: unsafe extern "C-unwind" fn(Id, crate::Sel) -> Id =
                crate::checked_msg();
            crate::ns_string_to_rust(description(id, crate::sel!(description)))
        });
        assert!(text.contains("aterm-objc test queue"), "{text}");
    }
}
