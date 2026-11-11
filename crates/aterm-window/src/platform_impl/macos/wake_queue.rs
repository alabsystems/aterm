// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
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

struct UnfairMutex<T> {
    word: UnsafeCell<OsUnfairLock>,
    value: UnsafeCell<T>,
}

// SAFETY: the value is reached only through a `Guard`, and a guard exists only
// while its thread holds the lock, so one thread at a time touches it; `T:
// Send` lets that thread be any thread.
unsafe impl<T: Send> Sync for UnfairMutex<T> {}

struct Guard<'a, T> {
    mutex: &'a UnfairMutex<T>,
    _locked_here: PhantomData<*const ()>,
}

impl<T> UnfairMutex<T> {
    const fn new(value: T) -> Self {
        UnfairMutex { word: UnsafeCell::new(OsUnfairLock(0)), value: UnsafeCell::new(value) }
    }

    fn lock(&self) -> Guard<'_, T> {
        // SAFETY: `word` is an initialised `os_unfair_lock` that stays put while
        // shared. No thread locks it twice: a guard's holder runs no code that
        // could send, and drops it before returning.
        unsafe { os_unfair_lock_lock(self.word.get()) };
        Guard { mutex: self, _locked_here: PhantomData }
    }

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

    fn arm(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    fn clear(&self) {
        self.0.swap(false, Ordering::AcqRel);
    }
}

/// What the receiver's latest take found, for the loop that turns on it.
#[derive(Clone, Debug)]
pub(super) struct Drained(Rc<Cell<bool>>);

impl Drained {
    /// Whether the latest take found the queue empty under its lock, so every event sent before
    /// that take began has been handed out. `false` after a take that found the lock held (a
    /// sender part-way through its push, whose signal brings the loop round again) or took a batch
    /// still being handed out.
    pub(super) fn emptied(&self) -> bool {
        self.0.get()
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
    drained: Drained,
    taken: RefCell<VecDeque<T>>,
}

/// A connected pair.
pub(super) fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let queue = Arc::new(UnfairMutex::new(Queue { events: VecDeque::new(), closed: false }));
    let armed = Armed::default();
    (Sender { queue: Arc::clone(&queue), armed: armed.clone() }, Receiver {
        queue,
        armed,
        drained: Drained(Rc::new(Cell::new(true))),
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

    /// [`Self::send`] by a sender held part-way through it, for a test that must make a take meet
    /// a send in progress: with the queue's lock taken it reports on `held` and waits for
    /// `release` (a one-minute hang detector) before it pushes. While it holds the lock it only
    /// signals and waits on those channels, never sends here.
    #[cfg(any(test, debug_assertions))]
    pub(super) fn send_held(
        &self,
        event: T,
        held: &std::sync::mpsc::Sender<()>,
        release: &std::sync::mpsc::Receiver<()>,
    ) -> Result<bool, T> {
        {
            let mut queue = self.queue.lock();
            let _ = held.send(());
            let _ = release.recv_timeout(std::time::Duration::from_secs(60));
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

    /// What the latest take found, for a loop that must know whether its drain handed out every
    /// event sent before it.
    pub(super) fn drained(&self) -> Drained {
        self.drained.clone()
    }

    fn refill(&self, taken: &mut VecDeque<T>) {
        if !taken.is_empty() {
            return;
        }
        self.armed.clear();
        let emptied = self.queue.try_lock().is_some_and(|mut queue| {
            mem::swap(taken, &mut queue.events);
            taken.is_empty()
        });
        self.drained.0.set(emptied);
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
#[path = "wake_queue_tests.rs"]
mod tests;
