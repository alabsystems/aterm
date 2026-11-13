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
    assert!(stop.set(), "the first set is the one that set it");
    for sleeper in sleepers {
        let (woken, slept) = sleeper.join().unwrap();
        assert!(
            woken && slept < HANG,
            "a sleeper slept past the stop ({slept:?})"
        );
    }
    assert!(stop.is_set());
    assert!(!stop.set(), "a second set changes nothing");
    assert!(stop.is_set());
    let from = std::time::Instant::now();
    assert!(stop.sleep(2 * HANG));
    assert!(
        from.elapsed() < HANG,
        "a sleep on a set stop ends at once, not at its time"
    );

    // Two setters race for one transition; exactly one owns it.
    let raced = Arc::new(Stop::new());
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let setters: Vec<_> = (0..2)
        .map(|_| {
            let stop = Arc::clone(&raced);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                stop.set()
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        setters
            .into_iter()
            .map(|setter| setter.join().unwrap())
            .filter(|first| *first)
            .count(),
        1
    );
    assert!(raced.is_set());
}

#[test]
fn an_unset_stop_sleeps_its_time_out() {
    let stop = Stop::new();
    assert!(!stop.sleep(Duration::from_millis(10)));
    assert!(!stop.sleep(Duration::ZERO));
    assert!(!stop.is_set());
}

/// A stop's bell polls ready once the stop is set and stays so: a thread in `poll(2)` on it wakes
/// when another thread sets the stop (one never rung reads no readiness at the hang detector's
/// minute), the bell is one pipe however often it is asked for, and a bell first asked for after
/// its stop was set is rung already. NEGATIVE CONTROL: the unset stop's bell, which a poll that
/// does not wait finds quiet.
#[cfg(unix)]
#[test]
fn a_stops_bell_rings_when_it_is_set_and_stays_rung() {
    use crate::ready::{Interest, Watch, any_interests_now, until};
    use std::os::fd::AsRawFd as _;

    let rung = |stop: &Stop| {
        any_interests_now(&[Watch::new(stop.bell().unwrap(), Interest::Read)])
            .unwrap()
            .len()
            == 1
    };
    let stop = Arc::new(Stop::new());
    assert!(!rung(&stop), "an unset stop's bell is quiet");
    assert_eq!(
        stop.bell().unwrap().as_raw_fd(),
        stop.bell().unwrap().as_raw_fd(),
        "a stop has one bell"
    );
    let poller = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            until(
                stop.bell().unwrap(),
                Interest::Read,
                std::time::Instant::now() + HANG,
            )
        })
    };
    assert!(stop.set());
    poller
        .join()
        .unwrap()
        .expect("the bell rang for the thread polling it");
    assert!(rung(&stop), "and stays rung");

    let late = Stop::new();
    assert!(late.set());
    assert!(
        rung(&late),
        "a bell first asked for after its stop was set is rung already"
    );
}

/// [`Stop::wait_ready`] with no deadline ends on what it waits for and on nothing else: a
/// descriptor gone readable ends it with that descriptor's index, and a stop set while it waits
/// ends it with none. A wait the stop fails to end runs to the hang detector's minute (its own
/// deadline), and reads as a wait that expired. Nothing is timed but that minute.
///
/// NEGATIVE CONTROL (2026-11-09): with the bell left out of the set `wait_ready` polls, the
/// second wait reached the hang detector (`a set stop ends the wait`).
#[cfg(unix)]
#[test]
fn a_wait_ready_ends_on_its_descriptor_or_its_stop() {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;

    let stop = Arc::new(Stop::new());
    let (quiet, _keep) = crate::pipe().unwrap();
    let (readable, mut source) = crate::pipe().unwrap();
    source.write_all(b"r").unwrap();
    assert_eq!(
        stop.wait_ready(&[quiet.as_fd(), readable.as_fd()], None)
            .unwrap(),
        [1],
        "a readable descriptor ends the wait"
    );
    let waiter = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let (quiet, _keep) = crate::pipe().unwrap();
            let entered = std::time::Instant::now();
            let ready = stop
                .wait_ready(&[quiet.as_fd()], Some(entered + HANG))
                .unwrap();
            (ready, entered.elapsed())
        })
    };
    assert!(stop.set());
    let (ready, waited) = waiter.join().unwrap();
    assert!(
        ready.is_empty() && waited < HANG,
        "a set stop ends the wait ({ready:?} after {waited:?})"
    );
}

/// A thread asleep on a stop waits in one condition-variable wait, where a thread that reads a
/// flag between short sleeps (the polling a stop replaces) sleeps in a timer's call and wakes
/// for nothing many times a second: read where each sleeps (`asleep`), beside a thread in a plain
/// `Condvar::wait_timeout` and one in the old 25 ms poll, so no call's name is written here. A
/// thread can be caught blocked on its way to its wait (a page fault, an allocator's lock), so each
/// place is the one a thread is seen in three reads running, and the stop's sleeper is read until
/// it waits where the plain waiter does (a sleeper that never does fails at the hang detector's
/// minute). Notifications that leave the predicate false make the sleeper recheck it. While the
/// poller wakes eight times, no stop wait expires: spurious wakeups are allowed, timer polling is
/// not.
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
    // Notifications need not change the predicate. A correct stop rechecks it and keeps
    // waiting, however often that happens; spurious wakeups have the same effect.
    for _ in 0..3 {
        let before = {
            let _guard = stop.latch.lock().unwrap();
            let before = stop.wakes();
            stop.rung.notify_all();
            before
        };
        let until = std::time::Instant::now() + HANG;
        while stop.wakes() <= before {
            assert!(
                std::time::Instant::now() < until,
                "the stop's sleeper did not recheck its predicate"
            );
            crate::deadline::poll_yield();
        }
    }
    assert!(!stop.is_set());

    let from = rounds.load(Ordering::Acquire);
    let until = std::time::Instant::now() + HANG;
    while rounds.load(Ordering::Acquire) < from + 8 {
        assert!(std::time::Instant::now() < until, "the poller never woke");
        crate::deadline::poll_yield();
    }
    assert_eq!(
        stop.expired(),
        0,
        "a stop wait expired while the poller woke eight times"
    );

    stop.set();
    *plain.0.lock().unwrap() = true;
    plain.1.notify_all();
    drop(quit);
    for handle in [stop_sleeper, condvar_waiter, poller] {
        handle.join().unwrap();
    }
}
