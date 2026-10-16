// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

// The queue behind `EventLoopProxy`, for both macOS loops (AppKit and
// `headless.rs`), in place of the `std::sync::mpsc` channel upstream used.
//
// WHY. The main thread drains the proxy's events, and a drain must never wait
// on a sender. `mpsc`'s `try_recv` does: it claims the next slot as soon as a
// sender has reserved it, and if that sender has not finished writing it yet
// (preempted, or allocating the next block) the receiver spins a moment and
// then calls `thread::yield_now`. On macOS that is `sched_yield`, i.e.
// `swtch_pri`, which DEPRESSES the calling thread's priority for one
// scheduler quantum (10 ms). On a busy machine every other runnable thread
// then runs first, so the main thread lost exactly 10 ms with no work done.
// Measured on 2026-10-16 with 10 tabs flooding output: most main-loop turns
// of 2 ms or more were that pause. The `user_event` handler had returned
// after about 1 us, and the thread then sat off-CPU for 10.0 ms before the
// next event was dispatched. That set the `max_turn_ms` of about 10 ms the
// round-1 stall runs could not explain.
//
// WHAT. A `Mutex<VecDeque>` that senders lock for one push. The receiver only
// ever `try_lock`s it. Every holder of the lock is a sender part-way through
// a push, and its `send_event` signals the loop's wake after the push. So a
// drain that finds the lock held can stop: that sender's signal brings the
// loop round again, and the drain takes the event then. No sender waits on
// the drain for longer than one swap, and nothing in the drain spins or
// yields.
//
// WHAT IT DOES NOT PROMISE. Senders do wait on each other, where `mpsc`'s
// senders mostly did not. A send blocks while another sender is inside its
// push, and that includes the main thread when a handler posts to its own
// loop. The push is one `push_back`, so the wait is short unless the holder
// is preempted while it holds the lock.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem;
use std::sync::{Arc, Mutex, PoisonError, TryLockError};

/// The shared state: the events not taken yet, and whether the receiver is
/// gone (a send after that hands its event back, as `mpsc::Sender::send`
/// does).
struct Queue<T> {
    events: VecDeque<T>,
    closed: bool,
}

/// The sending half: cloned into every proxy.
pub(super) struct Sender<T>(Arc<Mutex<Queue<T>>>);

/// The receiving half: owned by the event loop, on the main thread. `taken`
/// holds the events moved off the shared queue and not handed out yet.
pub(super) struct Receiver<T> {
    queue: Arc<Mutex<Queue<T>>>,
    taken: RefCell<VecDeque<T>>,
}

/// A connected pair.
pub(super) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let queue = Arc::new(Mutex::new(Queue { events: VecDeque::new(), closed: false }));
    (Sender(Arc::clone(&queue)), Receiver { queue, taken: RefCell::default() })
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender(Arc::clone(&self.0))
    }
}

impl<T> Sender<T> {
    /// Queue `event` behind every event sent before it; `Err(event)` once the
    /// receiver is gone. The caller signals the loop's wake after this
    /// returns (see the module notes: a drain that found the lock held relies
    /// on that signal).
    pub(super) fn send(&self, event: T) -> Result<(), T> {
        let mut queue = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.closed {
            return Err(event);
        }
        queue.events.push_back(event);
        Ok(())
    }
}

impl<T> Receiver<T> {
    /// The oldest event not handed out yet. `None` when there is none, or
    /// when the only ones left belong to a sender that still holds the lock:
    /// this never waits for it, and that sender's wake signal follows its
    /// push. Events sent while a drain runs are handed out after the ones
    /// taken before them, so a handler's events to itself belong to the same
    /// drain.
    pub(super) fn try_next(&self) -> Option<T> {
        let mut taken = self.taken.borrow_mut();
        if taken.is_empty() {
            self.take_into(&mut taken);
        }
        taken.pop_front()
    }

    /// Whether an event is waiting, as [`Self::try_next`] would see it.
    pub(super) fn has_pending(&self) -> bool {
        let mut taken = self.taken.borrow_mut();
        if taken.is_empty() {
            self.take_into(&mut taken);
        }
        !taken.is_empty()
    }

    /// Move every queued event, oldest first, onto the back of `into`, or
    /// nothing while a sender holds the lock.
    fn take_into(&self, into: &mut VecDeque<T>) {
        let mut queue = match self.queue.try_lock() {
            Ok(queue) => queue,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if into.is_empty() {
            // The usual case: a swap, so the lock is held for no copy, and the
            // two buffers' capacity is reused between drains.
            mem::swap(into, &mut queue.events);
        } else {
            into.append(&mut queue.events);
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let left = {
            let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.closed = true;
            mem::take(&mut queue.events)
        };
        // Dropped outside the lock: an event's own `Drop` may send again.
        drop(left);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A hang detector for something that must happen, not a latency budget.
    const MUST_HAPPEN: Duration = Duration::from_secs(60);

    fn drain(rx: &Receiver<u32>) -> Vec<u32> {
        std::iter::from_fn(|| rx.try_next()).collect()
    }

    #[test]
    fn events_arrive_oldest_first_across_senders_and_drains() {
        let (tx, rx) = channel();
        let other = tx.clone();
        tx.send(1).unwrap();
        other.send(2).unwrap();
        assert!(rx.has_pending());
        assert_eq!(rx.try_next(), Some(1));
        // Sent while the drain still holds 2: handed out after it.
        tx.send(3).unwrap();
        assert_eq!(drain(&rx), [2, 3]);
        assert!(!rx.has_pending());
        other.send(4).unwrap();
        assert_eq!(drain(&rx), [4]);
    }

    #[test]
    fn a_send_after_the_receiver_is_gone_hands_its_event_back() {
        let (tx, rx) = channel();
        tx.send(1).unwrap();
        drop(rx);
        assert_eq!(tx.send(2), Err(2));
    }

    /// THE DRAIN NEVER WAITS ON A SENDER PART-WAY THROUGH A PUSH: another
    /// thread holds the queue's lock, as a sender preempted inside `send`
    /// does, and the drain returns with nothing, at once. The event that
    /// sender then pushes is taken by the next drain. NEGATIVE CONTROL: a
    /// drain that waited for the lock would sit until the holder lets go,
    /// and the holder lets go only once the drain has returned, or after the
    /// one-minute hang detector; the drain would then take the event and the
    /// first assertion fails.
    #[test]
    fn a_drain_returns_at_once_while_a_sender_holds_the_lock() {
        let (tx, rx) = channel::<u32>();
        let (held_tx, held_rx) = mpsc::channel();
        let (drained_tx, drained_rx) = mpsc::channel::<()>();
        let queue = Arc::clone(&tx.0);
        let holder = std::thread::spawn(move || {
            let mut guard = queue.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = drained_rx.recv_timeout(MUST_HAPPEN);
            guard.events.push_back(7);
        });
        held_rx.recv_timeout(MUST_HAPPEN).expect("the holder took the lock");
        assert!(!rx.has_pending(), "the look waited for the held lock");
        assert_eq!(drain(&rx), Vec::<u32>::new(), "the drain waited for the held lock");
        drained_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(drain(&rx), [7]);
    }
}
