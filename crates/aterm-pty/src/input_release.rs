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

/// Remove a writer before its borrowed flags or thread handle can expire,
/// including when the write callback unwinds.
struct Writing<'a, T: Blocked> {
    registry: &'a Registry<T>,
    id: u64,
}

impl<T: Blocked> Drop for Writing<'_, T> {
    fn drop(&mut self) {
        self.registry.leave(self.id);
    }
}

/// A stop flag's identity in the registry: its address, live for as long as
/// any write tagged with it (the write borrows the flag).
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
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar};
    use std::thread;

    /// The hang detector for a release that must happen.
    const HANG: Duration = Duration::from_secs(60);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Phase {
        /// Not inside its write (before it, or after it).
        Out,
        /// Blocked inside its write.
        In,
        /// Its write was cancelled.
        Cancelled,
    }

    /// A stand-in for a writer thread: its "write" blocks until a cancel, and
    /// a cancel succeeds only while it is blocked, as `CancelSynchronousIo`
    /// does.
    #[derive(Clone)]
    struct Stand(Arc<StandState>);

    struct StandState {
        phase: Mutex<Phase>,
        moved: Condvar,
        /// Cancels that found the thread outside its write.
        missed: Mutex<usize>,
    }

    impl Stand {
        fn new() -> Self {
            Self(Arc::new(StandState {
                phase: Mutex::new(Phase::Out),
                moved: Condvar::new(),
                missed: Mutex::new(0),
            }))
        }

        /// The blocking write: in until a cancel; answers 0, nothing written.
        fn block(&self) -> io::Result<usize> {
            let mut p = self.0.phase.lock_or_recover();
            *p = Phase::In;
            self.0.moved.notify_all();
            let (mut p, waited) = self
                .0
                .moved
                .wait_timeout_while(p, HANG, |p| *p != Phase::Cancelled)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(!waited.timed_out(), "the write was never released");
            *p = Phase::Out;
            Ok(0)
        }

        fn wait_for(&self, phase: Phase) {
            let p = self.0.phase.lock_or_recover();
            let (_p, waited) = self
                .0
                .moved
                .wait_timeout_while(p, HANG, |p| *p != phase)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(!waited.timed_out(), "the writer never reached {phase:?}");
        }

        fn phase(&self) -> Phase {
            *self.0.phase.lock_or_recover()
        }

        fn missed(&self) -> usize {
            *self.0.missed.lock_or_recover()
        }
    }

    impl Blocked for Stand {
        fn cancel(&self) -> bool {
            let mut p = self.0.phase.lock_or_recover();
            if *p == Phase::In {
                *p = Phase::Cancelled;
                self.0.moved.notify_all();
                return true;
            }
            *self.0.missed.lock_or_recover() += 1;
            self.0.moved.notify_all();
            false
        }
    }

    /// A writer on its own thread, blocked in its write until released.
    fn blocked_writer(
        registry: &Arc<Registry<Stand>>,
        session: usize,
        severed: &Arc<AtomicBool>,
        stop: &Arc<AtomicBool>,
    ) -> (Stand, thread::JoinHandle<io::Result<usize>>) {
        let stand = Stand::new();
        let (r, sv, st, me) = (
            Arc::clone(registry),
            Arc::clone(severed),
            Arc::clone(stop),
            stand.clone(),
        );
        let writer = thread::spawn(move || {
            r.write(session, &sv, Some(&st), Some(me.clone()), || me.block())
        });
        stand.wait_for(Phase::In);
        (stand, writer)
    }

    /// A STOP RELEASES THE WRITE IT TAGS, AND ONLY THAT ONE. Two pastes block
    /// in their writes on one session; stopping the first releases it, with
    /// nothing written, and leaves the second blocked until its own stop.
    #[test]
    fn a_stop_releases_only_the_write_it_tags() {
        let registry = Arc::new(Registry::new());
        let severed = Arc::new(AtomicBool::new(false));
        let (a_stop, b_stop) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (a, a_writer) = blocked_writer(&registry, 1, &severed, &a_stop);
        let (b, b_writer) = blocked_writer(&registry, 1, &severed, &b_stop);

        a_stop.store(true, Ordering::Release);
        assert!(registry.release(registry.stopped(&a_stop), Duration::from_millis(1), HANG));
        assert_eq!(a_writer.join().expect("writer").expect("write"), 0);
        assert_eq!(
            b.phase(),
            Phase::In,
            "the other paste's write is not touched"
        );
        assert_eq!(a.phase(), Phase::Out);

        b_stop.store(true, Ordering::Release);
        assert!(registry.release(registry.stopped(&b_stop), Duration::from_millis(1), HANG));
        assert_eq!(b_writer.join().expect("writer").expect("write"), 0);
    }

    /// A SEVER RELEASES EVERY WRITE OF ITS SESSION AND BARS THE NEXT. Two
    /// writes block on session 1 and one on session 2; the sever of session 1
    /// releases both of its own, leaves session 2's, and a write that begins
    /// on session 1 afterwards never reaches its call.
    #[test]
    fn a_sever_releases_every_write_of_its_session_and_bars_later_ones() {
        let registry = Arc::new(Registry::new());
        let (severed_1, severed_2) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let no_stop = Arc::new(AtomicBool::new(false));
        let (_, w1) = blocked_writer(&registry, 1, &severed_1, &no_stop);
        let (_, w2) = blocked_writer(&registry, 1, &severed_1, &no_stop);
        let (other, w3) = blocked_writer(&registry, 2, &severed_2, &no_stop);

        let ids = registry.sever(1, &severed_1);
        assert_eq!(ids.len(), 2);
        assert!(registry.release(ids, Duration::from_millis(1), HANG));
        for w in [w1, w2] {
            assert_eq!(w.join().expect("writer").expect("write"), 0);
        }
        assert_eq!(
            other.phase(),
            Phase::In,
            "another session's write is not touched"
        );

        let mut called = false;
        let later = registry.write(1, &severed_1, None, Some(Stand::new()), || {
            called = true;
            Ok(5)
        });
        assert_eq!(later.expect("barred write"), 0);
        assert!(!called, "a write after the sever is never made");

        assert!(registry.release(
            registry.sever(2, &severed_2),
            Duration::from_millis(1),
            HANG
        ));
        assert_eq!(w3.join().expect("writer").expect("write"), 0);
    }

    /// A CANCEL THAT COMES BEFORE THE WRITE IS BLOCKED IS TRIED AGAIN. The
    /// writer has read its flags (clear) and is between that look and its
    /// write when the stop comes: the first cancel finds nothing to cancel,
    /// and the release keeps trying until the write it would have missed is
    /// blocked, then cancels it. RED with one round instead of the retry: the
    /// write blocks until the hang detector.
    #[test]
    fn a_cancel_that_comes_before_the_write_is_blocked_is_tried_again() {
        let registry = Arc::new(Registry::new());
        let severed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let stand = Stand::new();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let at_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = {
            let (r, sv, st, me, gate, at_gate) = (
                Arc::clone(&registry),
                Arc::clone(&severed),
                Arc::clone(&stop),
                stand.clone(),
                Arc::clone(&gate),
                Arc::clone(&at_gate),
            );
            thread::spawn(move || {
                r.write(1, &sv, Some(&st), Some(me.clone()), || {
                    *at_gate.0.lock_or_recover() = true;
                    at_gate.1.notify_all();
                    let open = gate.0.lock_or_recover();
                    let _open = gate
                        .1
                        .wait_timeout_while(open, HANG, |open| !*open)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    me.block()
                })
            })
        };
        {
            let reached = at_gate.0.lock_or_recover();
            let _reached = at_gate
                .1
                .wait_timeout_while(reached, HANG, |reached| !*reached)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }

        stop.store(true, Ordering::Release);
        let ids = registry.stopped(&stop);
        assert_eq!(
            ids.len(),
            1,
            "the writer is registered before its flag check"
        );
        let releaser = {
            let r = Arc::clone(&registry);
            thread::spawn(move || r.release(ids, Duration::from_millis(1), HANG))
        };
        let missed = {
            let p = stand.0.phase.lock_or_recover();
            let (p, _) = stand
                .0
                .moved
                .wait_timeout_while(p, HANG, |_| stand.missed() == 0)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(p);
            stand.missed()
        };
        assert!(missed >= 1, "the first cancel came before the write");
        *gate.0.lock_or_recover() = true;
        gate.1.notify_all();

        assert!(releaser.join().expect("releaser"), "the retry released it");
        assert_eq!(writer.join().expect("writer").expect("write"), 0);
    }

    /// A WRITE THAT REGISTERS AFTER ITS STOP WRITES NOTHING: the canceller
    /// found no writer to cancel, so the writer's own look at its flags is
    /// what keeps it out of a write nothing would release. RED without the
    /// look: the write is made.
    #[test]
    fn a_write_that_registers_after_its_stop_writes_nothing() {
        let registry: Registry<Stand> = Registry::new();
        let severed = AtomicBool::new(false);
        let stop = AtomicBool::new(true);
        assert!(registry.stopped(&stop).is_empty());
        let mut called = false;
        let wrote = registry.write(1, &severed, Some(&stop), Some(Stand::new()), || {
            called = true;
            Ok(3)
        });
        assert_eq!(wrote.expect("barred"), 0);
        assert!(!called);

        let clear = AtomicBool::new(false);
        let wrote = registry.write(1, &severed, Some(&clear), Some(Stand::new()), || Ok(3));
        assert_eq!(wrote.expect("write"), 3);
    }

    /// A WRITER THAT LEFT IS NEVER CANCELLED: a release of a write that
    /// already ended finds it gone and cancels nothing, so no later write of
    /// that thread can be hit.
    #[test]
    fn a_writer_that_left_is_never_cancelled() {
        let registry: Registry<Stand> = Registry::new();
        let severed = AtomicBool::new(false);
        let stop = AtomicBool::new(false);
        let stand = Stand::new();
        let mut ids = Vec::new();
        let wrote = registry.write(1, &severed, Some(&stop), Some(stand.clone()), || {
            ids = registry.pick(|_| true);
            Ok(2)
        });
        assert_eq!(wrote.expect("write"), 2);
        assert_eq!(ids.len(), 1);
        assert!(registry.release(ids, Duration::from_millis(1), HANG));
        assert_eq!(
            stand.missed(),
            0,
            "no cancel was issued to a thread that left"
        );

        let mut unwound_ids = Vec::new();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.write(1, &severed, Some(&stop), Some(stand.clone()), || {
                unwound_ids = registry.pick(|_| true);
                panic!("write callback unwound");
            })
        }));
        assert!(unwound.is_err());
        assert_eq!(unwound_ids.len(), 1, "the callback ran while registered");
        registry.cancel_round(&mut unwound_ids);
        assert!(
            unwound_ids.is_empty(),
            "an unwound writer has left the registry"
        );
        assert_eq!(stand.missed(), 0, "no cancel follows an unwound writer");
    }
}
