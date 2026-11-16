// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use aterm_spec::wait::{MUST_HAPPEN, until_within};

use super::*;

thread_local! {
    /// What a test runs on this thread between a ring's last unlock and its wake.
    static BEFORE_WAKE: Cell<Option<fn()>> = const { Cell::new(None) };
}

/// The seam [`Changed::wake`] runs: this thread's hook, if a test armed one.
pub(super) fn before_wake() {
    if let Some(hook) = BEFORE_WAKE.with(Cell::get) {
        hook();
    }
}

/// A slice of a millisecond, for a park that must be cut many times.
fn short(left: Duration) -> Duration {
    left.min(Duration::from_millis(1))
}

/// A slice that ends with no wake asks after the caller, then after the
/// flag, and parks again: the hang-up on its third ask ends the park after
/// exactly three waits, and a flag moved at its first ask ends it after
/// one. Both are counts, inside a window the hang detector bounds. A wake
/// asks neither.
#[test]
fn a_bare_slice_asks_only_after_the_caller_and_the_flag() {
    static ASKS: AtomicUsize = AtomicUsize::new(0);
    fn third_ask() -> bool {
        ASKS.fetch_add(1, Ordering::SeqCst) + 1 >= 3
    }
    fn there() -> bool {
        false
    }
    let until = Instant::now() + MUST_HAPPEN;
    let sleep = |waits: &mut usize, bound: Duration| {
        *waits += 1;
        std::thread::sleep(bound);
        false
    };

    let (mut waits, flag_reads) = (0, AtomicUsize::new(0));
    let hang_up = Lane {
        slice: short,
        hung_up: third_ask,
        bell: None,
    };
    let moved = || {
        flag_reads.fetch_add(1, Ordering::SeqCst);
        false
    };
    let woke = park(|b| sleep(&mut waits, b), until, hang_up, &moved);
    assert_eq!(woke, Woke::HungUp);
    assert_eq!(waits, 3, "one wait per ask");
    assert_eq!(
        flag_reads.load(Ordering::SeqCst),
        2,
        "the flag is read after each ask that found the caller there"
    );

    let mut waits = 0;
    let lane = Lane {
        slice: short,
        hung_up: there,
        bell: None,
    };
    let woke = park(|b| sleep(&mut waits, b), until, lane, &|| true);
    assert_eq!((woke, waits), (Woke::Rang, 1), "a moved flag is a wake");

    ASKS.store(0, Ordering::SeqCst);
    let unread = || -> bool { panic!("a wake reads no flag") };
    let woke = park(|_| true, until, hang_up, &unread);
    assert_eq!((woke, ASKS.load(Ordering::SeqCst)), (Woke::Rang, 0));
}

/// Off a serving lane the park is one wait for the whole remainder, and
/// ends `Due` on that wait's own timeout: nothing is asked. The fake wait
/// answers that its whole bound passed unwoken without sleeping it, and the
/// instant is the hang detector's, so no stall short of it comes before the
/// one wait.
#[test]
fn an_unserved_park_is_one_wait() {
    let mut bounds = Vec::new();
    let lane = Lane {
        slice: |left: Duration| left,
        hung_up: || -> bool { panic!("an unserved park asks after no caller") },
        bell: None,
    };
    let woke = park(
        |bound| {
            bounds.push(bound);
            false
        },
        Instant::now() + MUST_HAPPEN,
        lane,
        &|| -> bool { panic!("and reads no flag") },
    );
    assert_eq!((woke, bounds.len()), (Woke::Due, 1), "{bounds:?}");
}

/// A guarded park hands the guard back: a ring ends it with the state
/// the ringer wrote readable under the guard. The negative control, no
/// ring, ends at its instant with the guard back too.
#[test]
fn a_guarded_park_hands_its_guard_back() {
    let (state, changed) = (Mutex::new(0u32), Changed::new());
    let parked = AtomicBool::new(false);
    let lane = Lane {
        slice: short,
        hung_up: never,
        bell: None,
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            until_within(MUST_HAPPEN, "the park to hold the condvar", || {
                parked.load(Ordering::SeqCst).then_some(())
            });
            let mut held = state.lock().unwrap();
            *held = 7;
            changed.ring(held);
        });
        let until = Instant::now() + MUST_HAPPEN;
        let mut guard = state.lock().unwrap();
        parked.store(true, Ordering::SeqCst);
        while *guard != 7 {
            assert!(Instant::now() < until, "hang detector: the ring never came");
            guard = changed.park(guard, until, lane);
        }
    });

    let guard = state.lock().unwrap();
    let guard = changed.park(guard, Instant::now() + Duration::from_millis(5), lane);
    assert_eq!(*guard, 7, "the guard comes back at the instant");
}

/// A ring whose wake lands while a guarded park is between slices ends
/// the park. The order is forced: the ringer counts its change under the
/// lock and releases it, the park's slice times out and takes the lock
/// back, the park asks after its caller, and only during that ask does
/// the ringer's wake go out (the seam holds it back), finding nobody
/// waiting. The count, read after the ask, ends the park; a park that
/// lost the ring would wait another slice and ask again.
#[test]
fn a_ring_between_slices_ends_the_park() {
    /// 0 unrung; 1 counted and unlocked, its wake held back; 2 the park
    /// asks between slices; 3 the wake went out.
    static STAGE: AtomicU8 = AtomicU8::new(0);
    /// The asks after the wake went out.
    static LATE: AtomicUsize = AtomicUsize::new(0);
    fn between_slices() -> bool {
        match STAGE.load(Ordering::SeqCst) {
            0 => false,
            1 => {
                STAGE.store(2, Ordering::SeqCst);
                until_within(MUST_HAPPEN, "the ringer's wake", || {
                    (STAGE.load(Ordering::SeqCst) == 3).then_some(())
                });
                false
            }
            _ => {
                // The ring was lost: end the park now rather than at its instant.
                LATE.fetch_add(1, Ordering::SeqCst);
                true
            }
        }
    }
    fn held_back() {
        until_within(MUST_HAPPEN, "the park between slices", || {
            (STAGE.load(Ordering::SeqCst) == 2).then_some(())
        });
    }
    let (state, changed) = (Mutex::new(0u32), Changed::new());
    let parked = AtomicBool::new(false);
    let lane = Lane {
        slice: short,
        hung_up: between_slices,
        bell: None,
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            until_within(MUST_HAPPEN, "the park to hold the condvar", || {
                parked.load(Ordering::SeqCst).then_some(())
            });
            // The lock is free only while the park waits out a slice, and the park
            // reads the stage only under it: it sees 1 once the change is counted.
            let mut held = state.lock().unwrap();
            *held = 7;
            STAGE.store(1, Ordering::SeqCst);
            BEFORE_WAKE.set(Some(held_back));
            changed.ring(held);
            BEFORE_WAKE.set(None);
            STAGE.store(3, Ordering::SeqCst);
        });
        let until = Instant::now() + MUST_HAPPEN;
        let mut guard = state.lock().unwrap();
        parked.store(true, Ordering::SeqCst);
        while *guard != 7 {
            assert!(Instant::now() < until, "hang detector: the ring never came");
            guard = changed.park(guard, until, lane);
        }
        drop(guard);
        // A spurious wake answered before the ask: let the ringer finish.
        let _ = STAGE.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
    });
    assert_eq!(
        LATE.load(Ordering::SeqCst),
        0,
        "a park that missed the ring asked again"
    );
}

/// A park that begins while `unlocked` runs, its waiter finding the work
/// undone, ends once the work is done, though the ring's wake finds it
/// between slices: `ring_after` counts again, under the lock, after
/// `unlocked`. The order is forced: the waiter parks past the first
/// count while `unlocked` runs, `unlocked` then does its work, and the
/// ring's wake goes out (the seam holds it back) only while the park asks
/// between slices, finding nobody waiting. A ring that counted only
/// before `unlocked` leaves the park to wait another slice and ask again.
#[test]
fn a_park_begun_while_the_work_runs_ends_once_it_is_done() {
    /// 0 unrung; 1 `unlocked` runs; 2 the park began; 3 the work is done
    /// and its wake held back; 4 the park asks between slices; 5 the wake
    /// went out.
    static STAGE: AtomicU8 = AtomicU8::new(0);
    /// The work `unlocked` does, which the waiter reads.
    static DONE: AtomicBool = AtomicBool::new(false);
    /// The park ended before it asked with the wake held back.
    static ENDED: AtomicBool = AtomicBool::new(false);
    /// The asks after the wake went out.
    static LATE: AtomicUsize = AtomicUsize::new(0);
    fn between_slices() -> bool {
        match STAGE.load(Ordering::SeqCst) {
            3 => {
                STAGE.store(4, Ordering::SeqCst);
                until_within(MUST_HAPPEN, "the ringer's wake", || {
                    (STAGE.load(Ordering::SeqCst) == 5).then_some(())
                });
                false
            }
            5 => {
                // The work's count was lost: end the park now rather than at its instant.
                LATE.fetch_add(1, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }
    fn held_back() {
        STAGE.store(3, Ordering::SeqCst);
        until_within(MUST_HAPPEN, "the park between slices", || {
            (STAGE.load(Ordering::SeqCst) == 4 || ENDED.load(Ordering::SeqCst)).then_some(())
        });
    }
    let (state, changed) = (Mutex::new(()), Changed::new());
    let lane = Lane {
        slice: short,
        hung_up: between_slices,
        bell: None,
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            BEFORE_WAKE.set(Some(held_back));
            changed.ring_after(&state, state.lock().unwrap(), || {
                STAGE.store(1, Ordering::SeqCst);
                until_within(MUST_HAPPEN, "the park to begin", || {
                    (STAGE.load(Ordering::SeqCst) == 2).then_some(())
                });
                DONE.store(true, Ordering::SeqCst);
            });
            BEFORE_WAKE.set(None);
            STAGE.store(5, Ordering::SeqCst);
        });
        until_within(MUST_HAPPEN, "the work to begin", || {
            (STAGE.load(Ordering::SeqCst) == 1).then_some(())
        });
        let until = Instant::now() + MUST_HAPPEN;
        let mut guard = state.lock().unwrap();
        while !DONE.load(Ordering::SeqCst) {
            assert!(Instant::now() < until, "hang detector: the ring never came");
            // The work read undone, and the park's count read, under one hold of the lock.
            let _ = STAGE.compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
            guard = changed.park(guard, until, lane);
        }
        drop(guard);
        // The park ended before its ask with the wake held back (on the second count, read
        // before the seam ran, or on a spurious wake): let the ringer finish.
        ENDED.store(true, Ordering::SeqCst);
    });
    assert_eq!(
        LATE.load(Ordering::SeqCst),
        0,
        "a park that began before the work was done asked again after it"
    );
}

/// The guarded parks the hang-up bell rings: the caller of the served connection is a socket
/// pair's client, the lane a thread the test starts. Each wait for something that must happen is
/// the one-minute hang detector, and no count depends on time.
#[cfg(unix)]
mod belled {
    use std::os::fd::AsRawFd as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::CtlStream;
    use aterm_uds::hangup::Bell;

    use super::super::{BellLane, Changed, Lane};
    use crate::hangup::{ARMED_POLL, HANGUP_POLL, ServingFd, caller_hung_up, hangup_park};

    #[track_caller]
    fn until(what: &str, mut seen: impl FnMut() -> bool) {
        until_within(MUST_HAPPEN, what, || seen().then_some(()));
    }

    /// The shipped lanes: a served park arms the process's bell and, armed, waits at most
    /// ARMED_POLL at a time; a park that finishes whatever its caller does arms nothing; a slot
    /// park, which loses no ring, waits once for all that is left.
    #[test]
    fn a_served_condvar_park_arms_the_process_bell_and_caps_its_wait() {
        let long = Duration::from_secs(600);
        let bell = Lane::SERVED.bell.expect("a served park arms the bell");
        assert!(
            std::ptr::eq((bell.bell)(), Bell::process()),
            "the process's bell"
        );
        assert_eq!(
            (bell.slice)(long),
            ARMED_POLL,
            "armed, capped at ARMED_POLL"
        );
        assert_eq!((Lane::SERVED.slice)(long), long, "unserved, one wait");
        assert!(Lane::SERVED.ignoring_hangup().bell.is_none());
        let slotted = Lane::SERVED.slotted().bell.expect("still armed");
        assert_eq!((slotted.slice)(long), long, "a slot park is unsliced");
    }

    /// AN ARMED GUARDED PARK WAITS BY ITS BELL LANE, AN UNARMED ONE IS SLICED: each park runs
    /// over a served connection on a lane thread of its own, ten minutes from its own end. Its
    /// lane says the caller went once it has sliced twice. The park armed on a live bell asks its
    /// bell lane for one slice and its sliced lane for none, and the ring the test sends once it
    /// waits (taking the state's lock, which the park lets go only by waiting) ends it. The park
    /// whose bell has ended cannot arm, keeps the lane's slices, each no longer than HANGUP_POLL,
    /// and ends at the lane's second ask.
    /// NEGATIVE CONTROL: with the arm skipped, the live bell's park is sliced as well, (0, 2).
    #[test]
    fn an_armed_guarded_park_waits_by_its_bell_lane_where_an_unarmed_one_is_sliced() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        static SLICED: AtomicUsize = AtomicUsize::new(0);
        static LONGEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(left: Duration) -> Duration {
            SLICED.fetch_add(1, Ordering::SeqCst);
            let slice = hangup_park(left);
            let nanos = u64::try_from(slice.as_nanos()).unwrap_or(u64::MAX);
            LONGEST.fetch_max(nanos, Ordering::SeqCst);
            slice
        }
        fn gone_after_two_slices() -> bool {
            SLICED.load(Ordering::SeqCst) >= 2
        }
        fn counts() -> (usize, usize) {
            (BELLED.load(Ordering::SeqCst), SLICED.load(Ordering::SeqCst))
        }
        /// The counted park on `bell` over `server`, on a lane thread of its own, so a park that
        /// does not end is reported at the hang detector rather than after its ten minutes.
        fn counted_park(
            bell: fn() -> &'static Bell,
            server: CtlStream,
            state: Arc<(Mutex<u32>, Changed<u32>)>,
        ) -> std::thread::JoinHandle<u32> {
            BELLED.store(0, Ordering::SeqCst);
            SLICED.store(0, Ordering::SeqCst);
            LONGEST.store(0, Ordering::SeqCst);
            std::thread::spawn(move || {
                let _serving = ServingFd::enter(server.as_raw_fd());
                let lane = Lane {
                    slice: sliced,
                    hung_up: gone_after_two_slices,
                    bell: Some(BellLane {
                        bell,
                        slice: belled,
                    }),
                };
                let (held, changed) = &*state;
                let guard = held.lock().unwrap();
                let guard = changed.park(guard, Instant::now() + Duration::from_secs(600), lane);
                *guard
            })
        }

        let (_armed_client, armed_server) = CtlStream::pair().expect("socket pair");
        let state = Arc::new((Mutex::new(0u32), Changed::new()));
        let armed = counted_park(Bell::process, armed_server, Arc::clone(&state));
        until("the armed park to wait, or to end", || {
            counts().0 == 1 || armed.is_finished()
        });
        {
            let (held, changed) = &*state;
            let mut guard = held.lock().unwrap();
            *guard = 7;
            changed.ring(guard);
        }
        until("the armed park to end", || armed.is_finished());
        let seen = armed.join().expect("the armed park's lane");
        assert_eq!(
            (seen, counts()),
            (7, (1, 0)),
            "the armed park waits by its bell lane until the ring"
        );

        let (_ended_client, ended_server) = CtlStream::pair().expect("socket pair");
        let unarmed = counted_park(
            Bell::ended_for_test,
            ended_server,
            Arc::new((Mutex::new(0u32), Changed::new())),
        );
        until("the unarmed park to end", || unarmed.is_finished());
        let seen = unarmed.join().expect("the unarmed park's lane");
        assert_eq!(
            (seen, counts()),
            (0, (0, 2)),
            "a bell that has ended arms nothing, and the park is sliced"
        );
        let longest = Duration::from_nanos(LONGEST.load(Ordering::SeqCst));
        assert!(
            longest <= HANGUP_POLL,
            "a slice of {longest:?} outran HANGUP_POLL"
        );
    }

    /// A CALLER THAT HANGS UP RINGS AN ARMED GUARDED PARK: the park's one wait is ten minutes,
    /// and the caller closes only once the park waits (it called its bell lane under the state's
    /// lock, which the test then takes), so only the bell's notify of the park's own condvar can
    /// end that wait within the hang detector; the woken park's holder then finds its caller hung
    /// up.
    /// NEGATIVE CONTROL: arming the bell to notify a condvar that is not the park's leaves the
    /// park waiting past the hang detector.
    #[test]
    fn a_caller_that_hangs_up_rings_an_armed_guarded_park() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(_left: Duration) -> Duration {
            panic!("an armed park on a live bell is never sliced")
        }
        let (client, server) = CtlStream::pair().expect("socket pair");
        let state = Arc::new((Mutex::new(()), Changed::new()));
        let lane_state = Arc::clone(&state);
        let lane = std::thread::spawn(move || {
            let _serving = ServingFd::enter(server.as_raw_fd());
            let lane = Lane {
                slice: sliced,
                hung_up: caller_hung_up,
                bell: Some(BellLane {
                    bell: Bell::process,
                    slice: belled,
                }),
            };
            let until = Instant::now() + Duration::from_secs(600);
            let (held, changed) = &*lane_state;
            let mut guard = held.lock().unwrap();
            loop {
                if caller_hung_up() {
                    return true;
                }
                if Instant::now() >= until {
                    return false;
                }
                guard = changed.park(guard, until, lane);
            }
        });
        until("the armed park to wait", || {
            BELLED.load(Ordering::SeqCst) >= 1
        });
        drop(state.0.lock().unwrap());
        drop(client);
        until("the parked lane to hear its caller hang up", || {
            lane.is_finished()
        });
        assert!(lane.join().expect("the lane"), "the caller hung up");
    }

    /// ENTERING A SERVED CONNECTION STARTS THE PROCESS'S BELL, so a park on that thread arms it
    /// without starting it (an arm never starts a bell, and a start waits for every spawn in
    /// flight, which must not happen under the lock a park holds): on a thread inside
    /// ServingFd::enter, the process's bell arms a stream and serves it.
    /// NEGATIVE CONTROL: with the start taken out of ServingFd::enter, nothing in this test binary
    /// starts the process's bell, and the arm is refused.
    #[test]
    fn entering_a_served_connection_starts_the_process_bell() {
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let (near, _far) = CtlStream::pair().expect("socket pair");
        let (ring, _rung) = std::sync::mpsc::sync_channel(1);
        let _serving = ServingFd::enter(server.as_raw_fd());
        let armed = Bell::process()
            .arm(&near, ring)
            .expect("the served connection's entry started the process's bell");
        assert!(armed.ringing(), "and it serves the arm");
    }

    /// A BELL THAT ENDS MID-PARK LEAVES THE REST OF THE PARK SLICED, though its ending ring is
    /// lost: the park's first wait is armed on a bell of the test's own, and its bell lane, asked
    /// for that wait's bound under the state's lock, ends the bell's thread and waits until the
    /// bell arms nothing more, so the notify the ending bell sends finds no waiter (the park holds
    /// the lock, between its waits). That wait is a millisecond; every wait after it is bounded
    /// by the lane's own slice (HANGUP_POLL here, on a serving thread), which asks after the
    /// caller, and the lane says the caller went once it has sliced twice.
    /// NEGATIVE CONTROL: reading ringing() once, at the arm, keeps the bell lane's slice for every
    /// wait, and the lane says the caller went at the bell lane's third slice: (3, 0).
    #[test]
    fn a_bell_that_ends_mid_park_leaves_the_rest_of_the_park_sliced() {
        static ENDING: Bell = Bell::new();
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        static SLICED: AtomicUsize = AtomicUsize::new(0);
        fn ending() -> &'static Bell {
            &ENDING
        }
        fn belled(_left: Duration) -> Duration {
            if BELLED.fetch_add(1, Ordering::SeqCst) == 0 {
                ENDING.end_thread_for_test();
                let (probe, _peer) = CtlStream::pair().expect("socket pair");
                until("the bell to end", || {
                    let (ring, _rung) = std::sync::mpsc::sync_channel(1);
                    ENDING.arm(&probe, ring).is_err()
                });
            }
            Duration::from_millis(1)
        }
        fn sliced(left: Duration) -> Duration {
            SLICED.fetch_add(1, Ordering::SeqCst);
            hangup_park(left)
        }
        fn gone() -> bool {
            SLICED.load(Ordering::SeqCst) >= 2 || BELLED.load(Ordering::SeqCst) >= 3
        }
        assert!(ENDING.start(), "the test's bell starts");
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let lane = std::thread::spawn(move || {
            let _serving = ServingFd::enter(server.as_raw_fd());
            let lane = Lane {
                slice: sliced,
                hung_up: gone,
                bell: Some(BellLane {
                    bell: ending,
                    slice: belled,
                }),
            };
            let (state, changed) = (Mutex::new(()), Changed::new());
            let until = Instant::now() + Duration::from_secs(600);
            let mut guard = state.lock().unwrap();
            // A spurious wake hands the guard back early: park again until the lane says gone.
            while !gone() && Instant::now() < until {
                guard = changed.park(guard, until, lane);
            }
        });
        until("the park to end", || lane.is_finished());
        lane.join().expect("the park's lane");
        assert_eq!(
            (BELLED.load(Ordering::SeqCst), SLICED.load(Ordering::SeqCst)),
            (1, 2),
            "once its bell ended, the park is sliced by its lane"
        );
    }

    /// ONE CALLER'S HANG-UP WAKES EVERY PARK ON THEIR SHARED CONDVAR, so its own park hears it:
    /// two parks on one Changed (as every parked waiter of a session shares its Inbox's), each on
    /// a lane thread serving a socket pair of its own, each armed on the process's bell with one
    /// wait for all that is left. Once both wait (each called its bell lane under the state's
    /// lock, which the test then takes), the second's caller closes: its park ends within the
    /// hang detector, and the first, woken by the same ring, finds its caller there and parks
    /// again. The first's caller closes last.
    /// NEGATIVE CONTROL: a ring that wakes one waiter (Ring::Notify as notify_one) wakes only one
    /// of the two: either the second stays parked or the first never wakes, past the hang
    /// detector.
    #[test]
    fn a_hang_up_wakes_every_park_on_a_shared_condvar() {
        // Each park counts its own bell slices, so one park's wakes never stand for the other's.
        static FIRST_BELLED: AtomicUsize = AtomicUsize::new(0);
        static SECOND_BELLED: AtomicUsize = AtomicUsize::new(0);
        static FIRST_WOKE: AtomicUsize = AtomicUsize::new(0);
        fn first_belled(left: Duration) -> Duration {
            FIRST_BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn second_belled(left: Duration) -> Duration {
            SECOND_BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(_left: Duration) -> Duration {
            panic!("an armed park on a live bell is never sliced")
        }
        let state = Arc::new((Mutex::new(()), Changed::new()));
        let park_on = |server: CtlStream,
                       belled: fn(Duration) -> Duration,
                       woke: Option<&'static AtomicUsize>| {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                let _serving = ServingFd::enter(server.as_raw_fd());
                let lane = Lane {
                    slice: sliced,
                    hung_up: caller_hung_up,
                    bell: Some(BellLane {
                        bell: Bell::process,
                        slice: belled,
                    }),
                };
                let until = Instant::now() + Duration::from_secs(600);
                let (held, changed) = &*state;
                let mut guard = held.lock().unwrap();
                loop {
                    if caller_hung_up() {
                        return true;
                    }
                    if Instant::now() >= until {
                        return false;
                    }
                    guard = changed.park(guard, until, lane);
                    if let Some(woke) = woke {
                        woke.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        };
        let (first_client, first_server) = CtlStream::pair().expect("socket pair");
        let (second_client, second_server) = CtlStream::pair().expect("socket pair");
        // A park holds the lock from its arm through its slice until its wait lets the lock go,
        // so taking the lock after a park's slice shows that park armed and waiting: a notify
        // from then on reaches it.
        let first = park_on(first_server, first_belled, Some(&FIRST_WOKE));
        until("the first park to wait", || {
            FIRST_BELLED.load(Ordering::SeqCst) >= 1
        });
        let second = park_on(second_server, second_belled, None);
        until("the second park to wait", || {
            SECOND_BELLED.load(Ordering::SeqCst) >= 1
        });
        drop(state.0.lock().unwrap());
        drop(second_client);
        until("the second park to hear its caller hang up", || {
            second.is_finished()
        });
        assert!(
            second.join().expect("the second lane"),
            "its caller hung up"
        );
        until("the first park to wake at the same ring", || {
            FIRST_WOKE.load(Ordering::SeqCst) >= 1
        });
        until("the first park to wait again", || {
            FIRST_BELLED.load(Ordering::SeqCst) >= 2
        });
        drop(state.0.lock().unwrap());
        drop(first_client);
        until("the first park to hear its caller hang up", || {
            first.is_finished()
        });
        assert!(first.join().expect("the first lane"), "its caller hung up");
    }
}
