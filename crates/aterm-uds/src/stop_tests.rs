// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Stop;
use std::sync::Arc;
use std::time::Duration;

/// The one-minute hang detector: a sleeper the stop fails to wake sleeps this long and reads
/// `false`.
const HANG: Duration = Duration::from_secs(60);

/// Every sleeper wakes when the stop is set, not only one: the stop is set once all three wait
/// (each counts itself under the lock `set` takes, so none can miss the ring), and a sleeper that
/// was not rung reaches its deadline, the hang detector's minute.
#[test]
fn a_set_stop_wakes_every_sleeper_and_stays_set() {
    let stop = Arc::new(Stop::new());
    let sleepers: Vec<_> = (0..3)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let from = std::time::Instant::now();
                (stop.sleep(HANG), from.elapsed())
            })
        })
        .collect();
    let until = std::time::Instant::now() + HANG;
    while stop.entered() < 3 {
        assert!(
            std::time::Instant::now() < until,
            "the sleepers never waited"
        );
        crate::deadline::poll_yield();
    }
    assert!(!stop.is_set());
    stop.set();
    for sleeper in sleepers {
        let (woken, slept) = sleeper.join().unwrap();
        assert!(
            woken && slept < HANG,
            "a sleeper slept past the stop ({slept:?})"
        );
    }
    assert!(stop.is_set());
    stop.set();
    assert!(stop.is_set(), "a second set changes nothing");
    let from = std::time::Instant::now();
    assert!(stop.sleep(2 * HANG));
    assert!(
        from.elapsed() < HANG,
        "a sleep on a set stop ends at once, not at its time"
    );
}

#[test]
fn an_unset_stop_sleeps_its_time_out() {
    let stop = Stop::new();
    assert!(!stop.sleep(Duration::from_millis(10)));
    assert!(!stop.sleep(Duration::ZERO));
    assert!(!stop.is_set());
}

/// A thread asleep on a stop waits in one condition-variable wait, where a thread that reads a
/// flag between short sleeps (the polling a stop replaces) sleeps in a timer's call and wakes
/// for nothing many times a second: read where each sleeps (`asleep`), beside a thread in a plain
/// `Condvar::wait_timeout` and one in the old 25 ms poll, so no call's name is written here. A
/// thread can be caught blocked on its way to its wait (a page fault, an allocator's lock), so each
/// place is the one a thread is seen in three reads running, and the stop's sleeper is read until
/// it waits where the plain waiter does (a sleeper that never does fails at the hang detector's
/// minute). And it stays in that wait: while the poller wakes eight times, the stop's sleeper wakes
/// at most once (a spurious wakeup), which a stop that polled its condition variable would not.
/// NEGATIVE CONTROL: the poller, which sleeps where the stop's sleeper does not.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_sleeper_waits_in_one_condvar_wait_not_between_timer_sleeps() {
    use crate::asleep::{Asleep, Thread, asleep, this_thread};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex, mpsc};

    fn started(spawn: impl FnOnce() + Send + 'static) -> (Thread, std::thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            tx.send(this_thread()).unwrap();
            spawn();
        });
        (rx.recv().unwrap(), handle)
    }
    /// Where `thread` sleeps: the place three reads running find it in, within the hang
    /// detector's minute.
    fn settled(thread: Thread) -> Asleep {
        let until = std::time::Instant::now() + HANG;
        let mut seen: Option<(Asleep, usize)> = None;
        loop {
            let at = asleep(thread).unwrap();
            seen = match (seen, at) {
                (Some((last, reads)), Some(at)) if last == at => Some((last, reads + 1)),
                (_, at) => at.map(|at| (at, 1)),
            };
            if let Some((at, 3)) = &seen {
                return at.clone();
            }
            assert!(
                std::time::Instant::now() < until,
                "the thread never settled"
            );
            crate::deadline::poll_yield();
        }
    }
    /// Ends the poller however the test ends, so a failed assertion leaves no thread looping.
    struct Quit(Arc<AtomicBool>);
    impl Drop for Quit {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    let stop = Arc::new(Stop::new());
    let (on_stop, stop_sleeper) = {
        let stop = Arc::clone(&stop);
        started(move || assert!(stop.sleep(HANG)))
    };
    let plain = Arc::new((Mutex::new(false), Condvar::new()));
    let (in_condvar, condvar_waiter) = {
        let plain = Arc::clone(&plain);
        started(move || {
            let (woken, rung) = &*plain;
            let guard = woken.lock().unwrap();
            let (guard, _) = rung.wait_timeout_while(guard, HANG, |w| !*w).unwrap();
            assert!(*guard);
        })
    };
    let quit = Quit(Arc::new(AtomicBool::new(false)));
    let rounds = Arc::new(AtomicUsize::new(0));
    let (polling, poller) = {
        let quit = Arc::clone(&quit.0);
        let rounds = Arc::clone(&rounds);
        started(move || {
            while !quit.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(25));
                rounds.fetch_add(1, Ordering::Release);
            }
        })
    };

    let condvar_sleeps = settled(in_condvar);
    let poll_sleeps = settled(polling);
    let until = std::time::Instant::now() + HANG;
    while asleep(on_stop).unwrap().as_ref() != Some(&condvar_sleeps) {
        assert!(
            std::time::Instant::now() < until,
            "a stop's sleeper never waited where a plain condition-variable wait does ({:?})",
            asleep(on_stop).unwrap()
        );
        crate::deadline::poll_yield();
    }
    assert_ne!(
        poll_sleeps, condvar_sleeps,
        "the poller sleeps in a timer's call, which this test must tell apart"
    );
    let from = rounds.load(Ordering::Acquire);
    let until = std::time::Instant::now() + HANG;
    while rounds.load(Ordering::Acquire) < from + 8 {
        assert!(std::time::Instant::now() < until, "the poller never woke");
        crate::deadline::poll_yield();
    }
    assert!(
        stop.wakes() <= 1,
        "the stop's sleeper woke {} times while the poller woke eight",
        stop.wakes()
    );

    stop.set();
    *plain.0.lock().unwrap() = true;
    plain.1.notify_all();
    drop(quit);
    for handle in [stop_sleeper, condvar_waiter, poller] {
        handle.join().unwrap();
    }
}
