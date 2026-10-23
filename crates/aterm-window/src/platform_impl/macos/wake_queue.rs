// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::{RefCell, UnsafeCell};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// `os_unfair_lock` (`<os/lock.h>`, macOS 10.12): one 32-bit word, zero when
/// unlocked (`OS_UNFAIR_LOCK_INIT`).
#[repr(C)]
struct OsUnfairLock(u32);

unsafe extern "C" {
    fn os_unfair_lock_lock(lock: *mut OsUnfairLock);
    fn os_unfair_lock_trylock(lock: *mut OsUnfairLock) -> bool;
    fn os_unfair_lock_unlock(lock: *mut OsUnfairLock);
}

/// A value only the holder of its `os_unfair_lock` reaches. It is shared
/// only from inside an `Arc`, so the lock's word never moves while in use.
struct UnfairMutex<T> {
    word: UnsafeCell<OsUnfairLock>,
    value: UnsafeCell<T>,
}

// SAFETY: the value is reached only through a `Guard`, and a guard exists only while its thread holds the lock, so one thread at a time touches it.
unsafe impl<T: Send> Sync for UnfairMutex<T> {}

/// The lock, held. It unlocks on drop, on the thread that locked it: it is
/// `!Send`, since an unlock from any other thread aborts the process.
struct Guard<'a, T> {
    mutex: &'a UnfairMutex<T>,
    _locked_here: PhantomData<*const ()>,
}

impl<T> UnfairMutex<T> {
    const fn new(value: T) -> Self {
        UnfairMutex { word: UnsafeCell::new(OsUnfairLock(0)), value: UnsafeCell::new(value) }
    }

    /// Wait for the lock, lending this thread's priority to its holder
    /// (module notes).
    fn lock(&self) -> Guard<'_, T> {
        // SAFETY: `word` is an initialised `os_unfair_lock` that stays put while shared.
        unsafe { os_unfair_lock_lock(self.word.get()) };
        Guard { mutex: self, _locked_here: PhantomData }
    }

    /// The lock, if nobody holds it; never waits.
    fn try_lock(&self) -> Option<Guard<'_, T>> {
        // SAFETY: as in `lock`.
        let locked = unsafe { os_unfair_lock_trylock(self.word.get()) };
        locked.then(|| Guard { mutex: self, _locked_here: PhantomData })
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: this thread holds the lock (see `UnfairMutex`'s `Sync`).
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: this thread holds the lock, and `&mut self` is the only guard.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: this thread locked it (a guard is `!Send`) and unlocks it once.
        unsafe { os_unfair_lock_unlock(self.mutex.word.get()) };
    }
}

/// The shared state: the events not taken yet, and whether the receiver is
/// gone (a send after that hands its event back, as `mpsc::Sender::send`
/// does).
struct Queue<T> {
    events: VecDeque<T>,
    closed: bool,
}

/// Whether a send has armed the loop's wake since the last take.
#[derive(Clone, Debug, Default)]
pub(super) struct Armed(Arc<AtomicBool>);

impl Armed {
    /// Whether a send armed the wake after the last take.
    pub(super) fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Arm the wake; `true` when this call armed it, and so owes the signal.
    fn arm(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    /// Clear it, before a take. A read-modify-write, so it reads the last
    /// arm and sees every push made before it.
    fn clear(&self) {
        self.0.swap(false, Ordering::AcqRel);
    }
}

/// The sending half.
pub(super) struct Sender<T> {
    queue: Arc<UnfairMutex<Queue<T>>>,
    armed: Armed,
}

/// The receiving half.
pub(super) struct Receiver<T> {
    queue: Arc<UnfairMutex<Queue<T>>>,
    armed: Armed,
    taken: RefCell<VecDeque<T>>,
}

/// A connected pair.
pub(super) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let queue = Arc::new(UnfairMutex::new(Queue { events: VecDeque::new(), closed: false }));
    let armed = Armed::default();
    (Sender { queue: Arc::clone(&queue), armed: armed.clone() }, Receiver {
        queue,
        armed,
        taken: RefCell::default(),
    })
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender { queue: Arc::clone(&self.queue), armed: self.armed.clone() }
    }
}

impl<T> Sender<T> {
    /// Queue `event` behind every event sent before it.
    pub(super) fn send(&self, event: T) -> Result<bool, T> {
        {
            let mut queue = self.queue.lock();
            if queue.closed {
                return Err(event);
            }
            queue.events.push_back(event);
        }
        Ok(self.armed.arm())
    }
}

impl<T> Receiver<T> {
    /// The oldest event not handed out yet.
    pub(super) fn try_next(&self) -> Option<T> {
        let mut taken = self.taken.borrow_mut();
        self.refill(&mut taken);
        taken.pop_front()
    }

    /// Whether an event is waiting, as [`Self::try_next`] would see it.
    pub(super) fn has_pending(&self) -> bool {
        let mut taken = self.taken.borrow_mut();
        self.refill(&mut taken);
        !taken.is_empty()
    }

    /// The wake flag, for a loop that must know before it sleeps whether a send armed it after the last take.
    pub(super) fn armed(&self) -> Armed {
        self.armed.clone()
    }

    /// Once every taken event is handed out, clear the wake flag and take
    /// every queued one, oldest first; nothing while a sender holds the lock,
    /// whose arm then signals.
    fn refill(&self, taken: &mut VecDeque<T>) {
        if !taken.is_empty() {
            return;
        }
        self.armed.clear();
        if let Some(mut queue) = self.queue.try_lock() {
            mem::swap(taken, &mut queue.events);
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let left = {
            let mut queue = self.queue.lock();
            queue.closed = true;
            mem::take(&mut queue.events)
        };
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
        tx.send(3).unwrap();
        assert_eq!(drain(&rx), [2, 3]);
        assert!(!rx.has_pending());
        other.send(4).unwrap();
        assert_eq!(drain(&rx), [4]);
    }

    /// COALESCED WAKES: the first send after a take arms the wake and owes
    /// the signal; the sends behind it, from any sender, do not, until a
    /// drain takes again.
    #[test]
    fn only_the_send_that_arms_the_wake_owes_a_signal() {
        let (tx, rx) = channel();
        let other = tx.clone();
        let armed = rx.armed();
        assert!(!armed.is_set());
        assert_eq!(tx.send(1), Ok(true), "the first send signals");
        assert_eq!(other.send(2), Ok(false), "a drain is already owed");
        assert_eq!(tx.send(3), Ok(false));
        assert!(armed.is_set(), "owed a drain");
        assert_eq!(drain(&rx), [1, 2, 3]);
        assert!(!armed.is_set(), "the take cleared it");
        assert_eq!(other.send(4), Ok(true), "the next send after a take signals");
        assert_eq!(drain(&rx), [4]);
    }

    /// A DRAIN THAT TAKES NOTHING STILL CLEARS THE FLAG: with the lock held,
    /// as by a sender part-way through its push, the drain finds nothing, and
    /// the next arm still owes a signal, so the events queued behind the lock
    /// are drained.
    #[test]
    fn a_drain_that_finds_the_lock_held_still_clears_the_flag() {
        let (tx, rx) = channel::<u32>();
        assert_eq!(tx.send(1), Ok(true));
        let (held_tx, held_rx) = mpsc::channel();
        let (drained_tx, drained_rx) = mpsc::channel::<()>();
        let queue = Arc::clone(&tx.queue);
        let holder = std::thread::spawn(move || {
            let mut guard = queue.lock();
            held_tx.send(()).unwrap();
            let _ = drained_rx.recv_timeout(MUST_HAPPEN);
            guard.events.push_back(7);
        });
        held_rx.recv_timeout(MUST_HAPPEN).expect("the holder took the lock");
        assert_eq!(drain(&rx), Vec::<u32>::new(), "nothing taken past the held lock");
        drained_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(tx.send(8), Ok(true), "the drain cleared the flag: this send signals");
        assert_eq!(drain(&rx), [1, 7, 8]);
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
    /// does, and the drain returns with nothing, at once.
    #[test]
    fn a_drain_returns_at_once_while_a_sender_holds_the_lock() {
        let (tx, rx) = channel::<u32>();
        let (held_tx, held_rx) = mpsc::channel();
        let (drained_tx, drained_rx) = mpsc::channel::<()>();
        let queue = Arc::clone(&tx.queue);
        let holder = std::thread::spawn(move || {
            let mut guard = queue.lock();
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
