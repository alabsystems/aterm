// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::services::{FSEVENTSD, HOLD_MS, Service};

const GIB: u64 = 1 << 30;

fn reading(footprint: u64) -> Reading {
    Reading {
        total: 128 * GIB,
        services: vec![Service {
            coalition: 7,
            pid: 537,
            start_s: 1_790_000_000,
            uid: 0,
            name: "fseventsd".into(),
            path: Some(FSEVENTSD.path.into()),
            live: 1,
            footprint,
            ..Service::default()
        }],
        ..Reading::default()
    }
}

/// `(size, told, restart known)` of each service the note names.
fn heard(note: Option<HostNote>) -> Option<Vec<(Size, bool, bool)>> {
    match note? {
        HostNote::ServiceMemory { told, .. } => Some(
            told.services
                .iter()
                .map(|s| (s.size, s.told(), s.restart.is_some()))
                .collect(),
        ),
        other => panic!("{other:?}"),
    }
}

/// The window hears the first answer, the service followed before it is told, the edge
/// once the hold is met, a figure moved by a sixteenth, and the end; a failed reading, an
/// unchanged one and a figure moved by less tell nothing.
#[test]
fn the_window_hears_the_first_answer_each_change_and_the_end() {
    let mut b = Board::new(Some(true));
    let small = reading(4 << 20);
    let big = reading(40 * GIB);
    assert_eq!(heard(step(&mut b, 0, Some(&small))), Some(vec![]));
    assert_eq!(heard(step(&mut b, 60_000, Some(&small))), None);
    assert_eq!(heard(step(&mut b, 120_000, None)), None, "a failed reading");
    assert_eq!(
        heard(step(&mut b, 180_000, Some(&big))),
        Some(vec![(Size::Huge, false, true)]),
        "followed, not yet told"
    );
    assert_eq!(heard(step(&mut b, 240_000, Some(&big))), None);
    assert_eq!(
        heard(step(&mut b, 180_000 + HOLD_MS, Some(&big))),
        Some(vec![(Size::Huge, true, true)])
    );
    assert_eq!(
        heard(step(
            &mut b,
            240_000 + HOLD_MS,
            Some(&reading(40 * GIB + (1 << 20)))
        )),
        None,
        "a megabyte more is no news"
    );
    assert_eq!(
        heard(step(&mut b, 300_000 + HOLD_MS, Some(&reading(44 * GIB)))),
        Some(vec![(Size::Huge, true, true)]),
        "a tenth more is"
    );
    assert_eq!(
        heard(step(&mut b, 360_000 + HOLD_MS, Some(&small))),
        Some(vec![]),
        "the end"
    );
}

/// A press opens one restart's tab: another within five minutes opens nothing, nor does a
/// press for an account `sudo` would refuse. NEGATIVE CONTROL: five minutes on, a press
/// opens again.
#[test]
fn a_second_press_within_minutes_opens_nothing() {
    let t0 = Instant::now();
    assert_eq!(press_refused(None, t0, Some(true)), None);
    assert_eq!(press_refused(None, t0, None), None, "unread: sudo decides");
    assert!(press_refused(Some(t0), t0 + Duration::from_secs(60), Some(true)).is_some());
    assert!(press_refused(None, t0, Some(false)).is_some());
    assert_eq!(press_refused(Some(t0), t0 + PRESS_QUIET, Some(true)), None);
}

/// With no watch's reading (before the first, or in a process that does not watch), a press
/// reads the service now and opens the restart's tab from the margin under the large bar,
/// where `aterm init --only` restarts it. NEGATIVE CONTROL: under it, a service aterm knows
/// no restart for, and a failed read open nothing.
#[test]
fn a_press_with_no_reading_reads_the_service_now() {
    let at = |fp: u64| Some(reading(fp));
    assert!(read_holds("fseventsd", at(141 * GIB).as_ref()));
    assert!(read_holds("fseventsd", at(7 * GIB + (512 << 20)).as_ref()));
    assert!(!read_holds("fseventsd", at(6 * GIB).as_ref()));
    assert!(!read_holds("mds", at(141 * GIB).as_ref()));
    assert!(!read_holds("fseventsd", None));
}

/// A refusal names why, once, in the press's words.
#[test]
fn a_refused_press_says_why_once() {
    assert!(!refuse("fseventsd no longer holds past its bar"));
    assert_eq!(
        take_refusal().as_deref(),
        Some("Opened nothing: fseventsd no longer holds past its bar")
    );
    assert_eq!(take_refusal(), None);
}

#[test]
fn the_status_fields_say_dash_until_read() {
    let s = status_fields();
    assert!(s.starts_with("service_mem="), "{s}");
    assert!(s.contains(" service_mem_top="), "{s}");
    let b = Board::new(None);
    assert_eq!(b.judge.word(), "-");
}

/// THE WATCH RESTS FIVE MINUTES WHILE NOTHING NEEDS IT: a reading that follows no service and
/// finds memory not pressed rests [`SERVICES_CALM`]; a service at the margin under the large bar,
/// a told episode, the kernel's pressure past normal and no reading yet each read again in a
/// [`DISK_TICK`]. NEGATIVE CONTROLS: a constant `DISK_TICK` fails the calm row; a constant
/// `SERVICES_CALM` fails the followed row.
#[test]
fn the_watch_rests_long_only_while_nothing_is_followed() {
    let after = |r: &Reading| {
        let mut b = Board::new(Some(true));
        let _ = step(&mut b, 0, Some(r));
        next_read(&b)
    };
    assert_eq!(after(&reading(GIB)), SERVICES_CALM, "calm");
    assert_eq!(
        after(&reading(7 * GIB + (512 << 20))),
        DISK_TICK,
        "followed from the margin under the large bar"
    );
    assert_eq!(
        next_read(&Board::new(Some(true))),
        DISK_TICK,
        "no reading yet"
    );
    let mut b = Board::new(Some(true));
    let _ = step(&mut b, 0, Some(&reading(40 * GIB)));
    let _ = step(&mut b, HOLD_MS, Some(&reading(40 * GIB)));
    assert!(b.judge.over().next().is_some(), "told");
    assert_eq!(next_read(&b), DISK_TICK, "a told episode");
    let pressed = Reading {
        pressure_warn: Some(true),
        ..reading(GIB)
    };
    assert_eq!(after(&pressed), DISK_TICK, "pressure past normal");
}

/// A RING ENDS THE REST before its deadline, and the rest takes it: one heard before a five
/// minutes' rest ends it at once, and the next rest, given no time, runs to its deadline unrung.
/// A ring that reaches a parked rest is [`ParkBell`]'s own test
/// (`park_bell::tests::a_ring_reaches_a_parked_waiter`). NEGATIVE CONTROL: a rest that reads the
/// ring without taking it answers rung again.
#[test]
fn a_ring_ends_the_rest_once() {
    let bell = ParkBell::new(false);
    ring_on(&bell);
    assert!(rest_on(&bell, SERVICES_CALM), "rung before the rest");
    assert!(!rest_on(&bell, Duration::ZERO), "the ring was taken");
}

/// What the standby's loop did, in order.
#[derive(Debug, PartialEq, Eq)]
enum Did {
    /// It asked for the lock while another held it, having read `reads` times.
    Asked {
        reads: usize,
    },
    Read,
    Rested,
}

/// THE STANDBY WAITS ON THE LOCK, NOT ON A TIMER: while another process holds it, the watch asks
/// once (its take blocks), reads nothing and never rests; once the lock is free, a read follows
/// (a one-minute hang detector, not a budget). NEGATIVE CONTROL: a loop that rests before its
/// first take (a poll's sleep) rests while the lock is held.
#[test]
fn the_standby_blocks_on_the_lock_until_it_is_free() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    let (did, saw) = mpsc::channel();
    let (free, freed) = mpsc::channel::<()>();
    let asks = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let watch = std::thread::spawn({
        let (asks, reads) = (Arc::clone(&asks), Arc::clone(&reads));
        let (asked, read, rested) = (did.clone(), did.clone(), did);
        move || {
            watch_loop(
                || {
                    asks.fetch_add(1, Ordering::SeqCst);
                    let _ = asked.send(Did::Asked {
                        reads: reads.load(Ordering::SeqCst),
                    });
                    freed.recv().ok()
                },
                || {
                    reads.fetch_add(1, Ordering::SeqCst);
                    let _ = read.send(Did::Read);
                    (None, SERVICES_CALM)
                },
                |_| {},
                |_| {
                    let _ = rested.send(Did::Rested);
                    false
                },
            );
        }
    });
    let next = || {
        saw.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the loop moved")
    };
    assert_eq!(
        next(),
        Did::Asked { reads: 0 },
        "asked once, blocked, before any read or rest"
    );
    assert_eq!(asks.load(Ordering::SeqCst), 1);
    free.send(()).expect("free the lock");
    assert_eq!(next(), Did::Read, "a read follows the release");
    assert_eq!(next(), Did::Rested);
    watch.join().expect("the watch ends");
    assert_eq!(
        asks.load(Ordering::SeqCst),
        1,
        "one ask for the whole stand-by"
    );
}

/// A PRESS ANSWERS FROM THE WATCH'S READING, NEVER WITH A READ OF ITS OWN WHERE THE WATCH HAS
/// READ: a board that tells the service answers yes and one at rest (following nothing) answers
/// no, with no read on the press's thread, which would walk the process table on the window's;
/// only a board nothing has read takes a reading now. NEGATIVE CONTROL: a board at rest that is
/// read again at the press (as the freshness filter before this test read a board older than a
/// [`DISK_TICK`]) reads on the press's thread, and the count is 1 before the unread board's.
#[test]
fn a_press_answers_from_the_watchs_reading_without_a_read_of_its_own() {
    use std::cell::Cell;
    let told = Mutex::new(Board::new(Some(true)));
    let _ = step(
        &mut told.lock().expect("board"),
        0,
        Some(&reading(40 * GIB)),
    );
    let _ = step(
        &mut told.lock().expect("board"),
        HOLD_MS,
        Some(&reading(40 * GIB)),
    );
    let calm = Mutex::new(Board::new(Some(true)));
    let _ = step(&mut calm.lock().expect("board"), 0, Some(&reading(GIB)));
    assert_eq!(next_read(&calm.lock().expect("board")), SERVICES_CALM);
    let reads = Cell::new(0);
    let read = |footprint: u64| {
        reads.set(reads.get() + 1);
        Some(reading(footprint))
    };
    assert!(holds_in(&told, "fseventsd", || read(GIB)), "told");
    assert!(!holds_in(&calm, "fseventsd", || read(40 * GIB)), "at rest");
    assert_eq!(reads.get(), 0, "no read on the press's thread");
    let unread = Mutex::new(Board::new(Some(true)));
    assert!(holds_in(&unread, "fseventsd", || read(40 * GIB)), "unread");
    assert_eq!(reads.get(), 1, "a board nothing has read reads now");
}
