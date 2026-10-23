// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The one-future, one-thread executor native GPU init runs on (wgpu's
// adapter/device acquisition is `async` for the browser's sake). A future Ready on
// its first poll never touches the condvar. A wake from another thread (wgpu's
// driver callbacks) or from inside `poll` sets a latch the waiter consumes, so a
// wake that lands before the wait is not lost.

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
    const fn new() -> Self {
        Self {
            woken: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    /// Latch a wake and release the waiter (if any). Called from arbitrary
    /// threads, including this one, including re-entrantly from inside `poll`.
    fn notify(&self) {
        // Nothing panics while this guard is held, and poison cannot corrupt a
        // `bool`, so poison is ignored.
        let mut woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        *woken = true;
        drop(woken);
        // Exactly one thread ever waits on this condvar (the one inside
        // `block_on`), so a targeted wake is enough.
        self.cv.notify_one();
    }

    /// Block until a wake is latched, then consume the latch.
    fn wait(&self) {
        let mut woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        while !*woken {
            // Condvar waits may return spuriously.
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
/// Blocks, so it is native-only; the wasm renderer awaits the same futures
/// (see [`crate::GpuContext::from_instance`]). Between polls the thread
/// sleeps on a condvar rather than spinning.
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

    /// A future that parks and is woken from another thread (the adapter/device
    /// callback shape).
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

    /// A wake delivered inside `poll`, before the executor waits, sets the
    /// latch, so the next wait returns at once.
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
}
