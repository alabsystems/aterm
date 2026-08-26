// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The one-future, one-thread executor the NATIVE GPU init path runs on.
//
// wgpu's adapter/device acquisition is `async` because the browser has no other
// way to express it; natively there is no reactor, no task set and no
// concurrency — exactly one future, driven to completion on the calling thread,
// which is what startup wants. Retiring `pollster` for these ~40 lines removes a
// package from the shipped graph for a surface aterm uses three times.
//
// The contract, both halves of which the GPU path actually exercises:
//   * a future that is ALREADY `Ready` on its first poll returns without ever
//     touching the condvar (wgpu-core's `push_error_scope().pop()` resolves
//     immediately — no GPU round trip, no event loop);
//   * a future whose waker fires from ANOTHER thread (wgpu's device/adapter
//     callbacks land on a driver thread) wakes this one exactly once.
//
// The wake flag is CONSUMED by the waiter, not merely observed, so a wake that
// arrives *during* a poll — before this thread reaches the wait — is not lost:
// the next `wait()` sees the latch already set and returns immediately instead
// of sleeping for a wake that has already happened. That is the whole race, and
// the latch is the whole fix.

use std::future::Future;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

/// The park/unpark latch shared between the blocked thread and every clone of
/// the waker handed to the future.
struct Signal {
    /// `true` once a wake has been delivered and not yet consumed by a waiter.
    woken: Mutex<bool>,
    cv: Condvar,
}

impl Signal {
    fn new() -> Self {
        Self {
            woken: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    /// Latch a wake and release the waiter (if any). Called from arbitrary
    /// threads, including this one, including re-entrantly from inside `poll`.
    fn notify(&self) {
        // A poisoned lock cannot corrupt a `bool`: nothing here can panic while
        // the guard is held, so the only way to see poison is a panic in the
        // *waiter's* poll, which is already unwinding out of `block_on`.
        let mut woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        *woken = true;
        drop(woken);
        // Exactly one thread ever waits on this condvar (the one inside
        // `block_on`), so a targeted wake is enough.
        self.cv.notify_one();
    }

    /// Block until a wake is latched, then CONSUME the latch.
    fn wait(&self) {
        let mut woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        while !*woken {
            // Condvar waits are permitted to return spuriously, hence the loop
            // around the predicate rather than a bare wait.
            woken = self.cv.wait(woken).unwrap_or_else(PoisonError::into_inner);
        }
        *woken = false;
    }
}

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.notify();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

/// Drive `fut` to completion on the calling thread and return its output.
///
/// This BLOCKS. It is native-only by construction: blocking the browser's main
/// thread is forbidden, so the wasm renderer `.await`s the same futures instead
/// (see [`crate::GpuContext::from_instance`], which both paths share).
///
/// Between polls the thread sleeps on a condvar — no spinning, so an adapter
/// request that takes a driver-load's worth of milliseconds costs no CPU.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    // Pinned to this stack frame; `pin!` makes the future unreachable by name
    // afterwards, so it cannot be moved while polled and no `unsafe` is needed.
    let mut fut = std::pin::pin!(fut);
    let signal = Arc::new(Signal::new());
    let waker = Waker::from(Arc::clone(&signal));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => signal.wait(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::block_on;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    /// The `push_error_scope().pop()` shape: Ready on the very first poll, so
    /// the executor must never touch the condvar.
    #[test]
    fn already_ready_returns_without_parking() {
        assert_eq!(block_on(std::future::ready(7u32)), 7);
    }

    /// Counts polls so "Ready first time" is a claim about the executor, not
    /// about `std::future::ready`.
    #[test]
    fn ready_future_is_polled_exactly_once() {
        struct CountingReady(Arc<AtomicUsize>);
        impl Future for CountingReady {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Poll::Ready(())
            }
        }
        let polls = Arc::new(AtomicUsize::new(0));
        block_on(CountingReady(Arc::clone(&polls)));
        assert_eq!(polls.load(Ordering::Relaxed), 1);
    }

    /// A future that parks and is woken from ANOTHER thread — the adapter /
    /// device callback shape.
    #[test]
    fn wakes_from_another_thread() {
        #[derive(Default)]
        struct Shared {
            done: bool,
            waker: Option<Waker>,
        }
        struct Remote(Arc<Mutex<Shared>>);
        impl Future for Remote {
            type Output = u8;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u8> {
                let mut s = self.0.lock().expect("test mutex");
                if s.done {
                    Poll::Ready(42)
                } else {
                    s.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        let remote = Arc::clone(&shared);
        let joiner = std::thread::spawn(move || {
            // Spin until the main thread has registered a waker, so this test
            // exercises the sleep-then-wake path rather than the fast path.
            loop {
                let mut s = remote.lock().expect("test mutex");
                if let Some(w) = s.waker.take() {
                    s.done = true;
                    drop(s);
                    w.wake();
                    return;
                }
                drop(s);
                std::thread::yield_now();
            }
        });
        assert_eq!(block_on(Remote(shared)), 42);
        joiner.join().expect("waker thread");
    }

    /// A wake delivered from INSIDE `poll`, before the executor reaches its
    /// wait: the latch must already be set, so the next wait returns at once
    /// instead of sleeping forever for a wake that has been and gone.
    #[test]
    fn wake_during_poll_is_not_lost() {
        struct WakeThenPend(u8);
        impl Future for WakeThenPend {
            type Output = u8;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u8> {
                if self.0 == 0 {
                    return Poll::Ready(9);
                }
                self.0 -= 1;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
        assert_eq!(block_on(WakeThenPend(3)), 9);
    }

    /// Several wakes before a single wait must not leave a surplus latch that
    /// turns the NEXT park into a busy spin (the token-accounting bug a bare
    /// `thread::park`/`unpark` pair invites).
    #[test]
    fn surplus_wakes_collapse_to_one() {
        struct Storm(u8);
        impl Future for Storm {
            type Output = u8;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u8> {
                if self.0 == 0 {
                    return Poll::Ready(1);
                }
                self.0 -= 1;
                for _ in 0..4 {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
        }
        assert_eq!(block_on(Storm(2)), 1);
    }

    /// Waker clones outliving the future must stay sound (wgpu hands the waker
    /// to a callback that can fire after the poll that registered it).
    #[test]
    fn cloned_waker_outlives_the_poll() {
        let stash: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));
        let keep = Arc::clone(&stash);
        struct Stash(Arc<Mutex<Option<Waker>>>, bool);
        impl Future for Stash {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.1 {
                    return Poll::Ready(());
                }
                self.1 = true;
                *self.0.lock().expect("test mutex") = Some(cx.waker().clone());
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
        block_on(Stash(keep, false));
        // The stashed clone is still live here; waking it must be a no-op that
        // does not touch freed memory.
        if let Some(w) = stash.lock().expect("test mutex").take() {
            w.wake();
        }
    }
}
