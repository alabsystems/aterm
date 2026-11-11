// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Releasing an input writer blocked inside a synchronous write.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_types::MutexExt;

/// A registered writer's thread, which a cancel can release from its write.
pub(crate) trait Blocked {
    /// Cancels the synchronous write this thread is blocked in.
    fn cancel(&self) -> bool;
}

/// One writer inside [`Registry::write`].
struct Entry<T> {
    id: u64,
    /// The session's identity (the Windows backend's session address).
    session: usize,
    /// The address of the frame's stop flag; 0 for a write no stop reaches.
    stop: usize,
    thread: T,
}

struct Writers<T> {
    next: u64,
    entries: Vec<Entry<T>>,
}

/// Registry of writers currently inside a synchronous input write.
pub(crate) struct Registry<T> {
    writers: Mutex<Writers<T>>,
}

/// Removes a writer on drop, including when the write callback unwinds.
struct Writing<'a, T: Blocked> {
    registry: &'a Registry<T>,
    id: u64,
}

impl<T: Blocked> Drop for Writing<'_, T> {
    fn drop(&mut self) {
        self.registry.leave(self.id);
    }
}

/// A stop flag's address in the registry, live for as long as any write borrowing it.
fn stop_tag(stop: &AtomicBool) -> usize {
    std::ptr::from_ref(stop) as usize
}

impl<T: Blocked> Registry<T> {
    /// Creates an empty registry.
    pub(crate) const fn new() -> Self {
        Self {
            writers: Mutex::new(Writers {
                next: 0,
                entries: Vec::new(),
            }),
        }
    }

    /// Registers `thread` before checking `severed`/`stop`, then runs `write` if neither is set.
    pub(crate) fn write(
        &self,
        session: usize,
        severed: &AtomicBool,
        stop: Option<&AtomicBool>,
        thread: Option<T>,
        write: impl FnOnce() -> io::Result<usize>,
    ) -> io::Result<usize> {
        let _writing = thread.map(|thread| Writing {
            registry: self,
            id: self.enter(session, stop.map_or(0, stop_tag), thread),
        });
        let barred =
            severed.load(Ordering::Acquire) || stop.is_some_and(|s| s.load(Ordering::Acquire));
        if barred { Ok(0) } else { write() }
    }

    fn enter(&self, session: usize, stop: usize, thread: T) -> u64 {
        let mut w = self.writers.lock_or_recover();
        let id = w.next;
        w.next = w.next.wrapping_add(1);
        w.entries.push(Entry {
            id,
            session,
            stop,
            thread,
        });
        id
    }

    fn leave(&self, id: u64) {
        self.writers
            .lock_or_recover()
            .entries
            .retain(|e| e.id != id);
    }

    /// Bars later writes on `session` and returns in-flight write IDs for [`Self::release`].
    pub(crate) fn sever(&self, session: usize, severed: &AtomicBool) -> Vec<u64> {
        severed.store(true, Ordering::Release);
        self.pick(|e| e.session == session)
    }

    /// Returns in-flight write IDs tagged with `stop` for [`Self::release`].
    pub(crate) fn stopped(&self, stop: &AtomicBool) -> Vec<u64> {
        let tag = stop_tag(stop);
        self.pick(|e| e.stop == tag)
    }

    fn pick(&self, which: impl Fn(&Entry<T>) -> bool) -> Vec<u64> {
        let w = self.writers.lock_or_recover();
        w.entries
            .iter()
            .filter(|e| which(e))
            .map(|e| e.id)
            .collect()
    }

    /// Cancels each still-registered writer in `ids`, retaining those not yet blocked.
    pub(crate) fn cancel_round(&self, ids: &mut Vec<u64>) {
        let w = self.writers.lock_or_recover();
        ids.retain(|id| {
            w.entries
                .iter()
                .find(|e| e.id == *id)
                .is_some_and(|e| !e.thread.cancel())
        });
    }

    /// Repeatedly cancels `ids` every `step` up to `bound` until all are cancelled or have left.
    pub(crate) fn release(&self, mut ids: Vec<u64>, step: Duration, bound: Duration) -> bool {
        let start = Instant::now();
        loop {
            self.cancel_round(&mut ids);
            if ids.is_empty() {
                return true;
            }
            if start.elapsed() >= bound {
                return false;
            }
            std::thread::sleep(step);
        }
    }
}

#[cfg(test)]
#[path = "input_release_tests.rs"]
mod tests;
