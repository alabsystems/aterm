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
// stall runs could not explain.
//
// WHAT. A `VecDeque` behind an `os_unfair_lock` that senders take for one
// push. The receiver only ever try-locks it. Every holder of the lock is a
// sender part-way through a push, and its `send_event` signals the loop's
// wake after the push. So a drain that finds the lock held can stop: that
// sender's signal brings the loop round again, and the drain takes the event
// then. No sender waits on the drain for longer than one swap, and nothing in
// the drain spins or yields.
//
// SENDERS WAIT ON EACH OTHER. A send blocks while another sender is inside its
// push, and that includes the main thread when a handler posts to its own
// loop. The push is one `push_back`, so the wait is short unless the holder is
// preempted while it holds the lock: a lower-class sender (a UTILITY worker)
// pushed aside by middle-class work (PTY readers flooding, a build). So the
// lock is an `os_unfair_lock`: a thread that waits on it names its owner to the
// kernel, which runs the owner at the waiter's priority until it unlocks.
// `std::sync::Mutex` (a pthread mutex here) lends the waiter's priority too
// (a BACKGROUND holder ran at 31 while a waiter at 31 waited, with either
// lock, and stayed at 4 with no waiter), yet under contention it left a
// main-thread send waiting more often and far longer. Measured on 2026-10-16
// (`wqbench`: BACKGROUND threads sending in a loop under UTILITY spinners on
// 14 cores, the timed thread USER_INTERACTIVE, two shapes, interleaved): sends
// that waited over 1 ms, 44 with the pthread mutex and 9 with this lock in
// 400,000 timed sends per lock, and 28 and 18 in a second run of 160,000; the
// longest, 90.8 ms and 2.96 ms, then 66.9 ms and 2.96 ms. The median contended
// send costs 0.17 us here against 0.04 us, and an uncontended one 0.04 us with
// either.
//
// COALESCED WAKES (2026-10-17 sweep item 39). A wake is a
// `CFRunLoopSourceSignal` plus a `CFRunLoopWakeUp`, which sends a mach message
// holding the run loop's own lock, the lock the main thread takes on every
// turn. Signalled per send, a 10-tab output flood made one wake per event,
// about 3.6 per turn (235,162 events over 65,460 turns), and the main thread
// waited on that lock for about 6 ms per 9 s flood. So only the send that ARMS
// the wake signals: `Armed` is clear after a take, the first send after it
// sets it and signals, and the rest find it set and do not. The guarantee is
// unchanged, every push is followed by a drain:
//
// * The drain clears the flag BEFORE each take, whatever the take finds. A
//   send whose arm comes after the clear signals; a send whose arm came
//   before it pushed before it too, so the take finds its event, unless the
//   lock was held, and then that holder's own arm, after its push and after
//   the clear, finds the flag clear and signals. Both are read-modify-writes
//   (acquire-release), so a clear that reads a set flag sees every push of
//   every send that set it since the last clear.
// * Every signal the run loop handles is followed by a drain: the display-free
//   loop drains after every run and looks (`has_pending`, a take) before every
//   wait; the AppKit loop drains in every `BeforeWaiting` and, at the end of
//   that turn, wakes itself once more if a send armed the flag after its drain
//   (a nested run loop in a later handler, an app-modal dialog, may have
//   handled that send's signal with no drain after it).

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

// libSystem, which every macOS binary links.
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

// SAFETY: the value is reached only through a `Guard`, and a guard exists only
// while its thread holds the lock, so one thread at a time touches it; `T:
// Send` lets that thread be any thread.
unsafe impl<T: Send> Sync for UnfairMutex<T> {}

/// The lock, held. It unlocks on drop, on the thread that locked it: it is
/// `!Send`, since an unlock from any other thread aborts the process.
struct Guard<'a, T> {
    mutex: &'a UnfairMutex<T>,
    _locked_here: PhantomData<*const ()>,
}

impl<T> UnfairMutex<T> {
    fn new(value: T) -> Self {
        UnfairMutex { word: UnsafeCell::new(OsUnfairLock(0)), value: UnsafeCell::new(value) }
    }

    /// Wait for the lock, lending this thread's priority to its holder
    /// (module notes).
    fn lock(&self) -> Guard<'_, T> {
        // SAFETY: `word` is an initialised `os_unfair_lock` that stays put while
        // shared. No thread locks it twice: a guard's holder runs no code that
        // could send, and drops it before returning.
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

/// Whether a send has armed the loop's wake since the last take: set by the
/// send that signals, cleared before every take (module notes, COALESCED
/// WAKES).
#[derive(Clone, Debug, Default)]
pub(super) struct Armed(Arc<AtomicBool>);

impl Armed {
    /// Whether a send armed the wake after the last take: the loop is owed a
    /// drain, though the signal may have been handled already.
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

/// The sending half: cloned into every proxy.
pub(super) struct Sender<T> {
    queue: Arc<UnfairMutex<Queue<T>>>,
    armed: Armed,
}

/// The receiving half: owned by the event loop, on the main thread. `taken`
/// holds the events moved off the shared queue and not handed out yet.
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
    /// Queue `event` behind every event sent before it; `Err(event)` once the
    /// receiver is gone. `Ok(true)` when this send armed the wake: the caller
    /// signals the loop then, and a drain that found the lock held relies on
    /// that signal. `Ok(false)`: an earlier send's signal is still owed a
    /// drain, and that drain takes this event (module notes).
    pub(super) fn send(&self, event: T) -> Result<bool, T> {
        {
            let mut queue = self.queue.lock();
            if queue.closed {
                return Err(event);
            }
            queue.events.push_back(event);
        }
        // After the push and outside the lock: a drain that found the lock
        // held cleared the flag first, so this arm finds it clear.
        Ok(self.armed.arm())
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
        self.refill(&mut taken);
        taken.pop_front()
    }

    /// Whether an event is waiting, as [`Self::try_next`] would see it.
    pub(super) fn has_pending(&self) -> bool {
        let mut taken = self.taken.borrow_mut();
        self.refill(&mut taken);
        !taken.is_empty()
    }

    /// The wake flag, for a loop that must know before it sleeps whether a
    /// send armed it after the last take.
    pub(super) fn armed(&self) -> Armed {
        self.armed.clone()
    }

    /// Once every taken event is handed out, clear the wake flag and take
    /// every queued one, oldest first; nothing while a sender holds the lock,
    /// whose arm then signals. A swap, so the lock is held for no copy, and
    /// the two buffers' capacity is reused between drains.
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

    /// COALESCED WAKES: the first send after a take arms the wake and owes
    /// the signal; the sends behind it, from any sender, do not, until a
    /// drain takes again. NEGATIVE CONTROL: the per-send signal this replaced
    /// (`arm` always owing one) fails the second assertion.
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
    /// are drained. NEGATIVE CONTROL: a flag cleared only by a take that got
    /// the lock stays set, the send after the holder owes no signal, and its
    /// event and the held ones wait for an unrelated wake (here: forever).
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
