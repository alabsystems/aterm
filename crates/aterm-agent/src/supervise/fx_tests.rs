// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The door's clock sources against what they replaced.

use super::*;
use std::sync::atomic::{AtomicI64, Ordering::SeqCst};

/// THE ORACLE: the fixed clock as `Session` kept it before the door — its
/// five `cfg(test)` fields and the `cfg(test)` branches of `now_unix` and
/// `now_unix_ms`, copied as they were (atomics, `swap` and all), standing
/// still (the running clock is checked apart: its real elapsed time differs
/// between two clocks by construction).
struct OldKnobs {
    clock: Option<i64>,
    clock_jump: AtomicI64,
    clock_skew: AtomicI64,
    clock_phase_ms: i64,
}

impl OldKnobs {
    fn new() -> Self {
        Self {
            clock: None,
            clock_jump: AtomicI64::new(0),
            clock_skew: AtomicI64::new(0),
            clock_phase_ms: 0,
        }
    }
    fn set_clock_at_ms(&mut self, now_ms: i64) {
        self.clock = Some(now_ms.div_euclid(1000));
        self.clock_phase_ms = now_ms.rem_euclid(1000);
    }
    fn set_clock_jump(&self, jump: i64) {
        self.clock_jump.store(jump, SeqCst);
        self.clock_skew.store(0, SeqCst);
    }
    fn now_unix(&self) -> i64 {
        if self.clock.is_some() {
            return self.now_unix_ms().div_euclid(1000);
        }
        unreachable!("the oracle only answers for a fixed clock")
    }
    fn now_unix_ms(&self) -> i64 {
        let fixed = self
            .clock
            .expect("the oracle only answers for a fixed clock");
        let jump = self.clock_jump.load(SeqCst);
        let secs = fixed + self.clock_skew.swap(jump, SeqCst);
        secs * 1000 + self.clock_phase_ms
    }
}

/// One step of a script both clocks run.
#[derive(Clone, Copy, Debug)]
enum Step {
    Fix(i64),
    Jump(i64),
    ReadMs,
    ReadS,
    Peek,
}

/// The test suite's own wall instant (`TEST_NOW`, 2026-05-19 14:00 PDT).
const NOW: i64 = 1_779_224_400;

/// A fixed clock under the scripts the Mac-sleep tests stage — a sleep of four
/// hours after the wait's first read, an ordinary tick, a clock stepped BACK,
/// a second jump staged before the first was read, a fix to the millisecond
/// just short of a second's turn, a jump staged before the clock is fixed —
/// reads, read by read, as the old fields did: the same milliseconds, the
/// same seconds, the jump swapped in at the same read. A `peek` reads the
/// clock without making that read (the oracle is not asked: it had none).
#[test]
fn scripted_clock_matches_the_old_knobs() {
    use Step::{Fix, Jump, Peek, ReadMs, ReadS};
    let scripts: &[(&str, &[Step])] = &[
        (
            "a four-hour sleep after the wait's first read",
            &[
                Fix(NOW * 1000),
                Jump(4 * 3600),
                ReadMs,
                ReadMs,
                ReadS,
                ReadMs,
            ],
        ),
        (
            "an ordinary step's tick",
            &[Fix(NOW * 1000), Jump(20), ReadS, ReadS, ReadMs],
        ),
        (
            "a clock stepped back",
            &[Fix(NOW * 1000), Jump(-3600), ReadMs, ReadMs, ReadS],
        ),
        (
            "a second jump staged before the first was read, then after",
            &[
                Fix(NOW * 1000),
                Jump(60),
                Jump(7200),
                ReadMs,
                ReadMs,
                Jump(5),
                ReadMs,
                ReadMs,
            ],
        ),
        (
            "a fix to the millisecond, just short of a second's turn",
            &[
                Fix(NOW * 1000 + 999),
                ReadMs,
                ReadS,
                Jump(1),
                ReadS,
                ReadS,
                ReadMs,
            ],
        ),
        (
            "a jump staged before the clock is fixed",
            &[Jump(90), Fix(NOW * 1000 + 250), ReadMs, ReadMs, ReadS],
        ),
        (
            "peeks around the staged jump",
            &[
                Fix(NOW * 1000),
                Jump(4 * 3600),
                Peek,
                Peek,
                ReadMs,
                Peek,
                ReadMs,
            ],
        ),
    ];
    for (what, script) in scripts {
        let mut old = OldKnobs::new();
        let mut fx = Fx::os();
        let mut reads = Vec::new();
        for (at, step) in script.iter().enumerate() {
            let clock = fx
                .scripted()
                .expect("the system's clock becomes a scripted one");
            match *step {
                Fix(ms) => {
                    old.set_clock_at_ms(ms);
                    clock.set_at_ms(ms);
                }
                Jump(s) => {
                    old.set_clock_jump(s);
                    clock.set_jump(s);
                }
                ReadMs => {
                    let (want, got) = (old.now_unix_ms(), fx.wall_ms());
                    assert_eq!(got, want, "{what}: step {at} {step:?}");
                    reads.push(got);
                }
                ReadS => {
                    let (want, got) = (old.now_unix(), fx.wall_s());
                    assert_eq!(got, want, "{what}: step {at} {step:?}");
                    reads.push(got * 1000);
                }
                Peek => {
                    // What the next read will say, staged jump included —
                    // and asking changes nothing: the oracle's next read
                    // still agrees with the clock's next read.
                    let peeked = clock.peek_wall_ms();
                    assert_eq!(peeked, clock.peek_wall_ms(), "{what}: a peek is stable");
                    reads.push(peeked);
                }
            }
        }
        assert!(!reads.is_empty(), "{what}: the script reads the clock");
    }
}

/// The swap the Mac-sleep tests stand on, spelled out once: after
/// `set_jump`, the next read still says the fixed time and every later one the
/// jump; a peek in between sees the time the next read will say and moves
/// nothing.
#[test]
fn a_staged_jump_lands_after_the_next_read_and_a_peek_lands_nothing() {
    let mut fx = Fx::os();
    let clock = fx.scripted().expect("scripted");
    clock.set_at_ms(NOW * 1000);
    clock.set_jump(4 * 3600);
    assert_eq!(clock.peek_wall_ms(), NOW * 1000);
    assert_eq!(
        fx.wall_ms(),
        NOW * 1000,
        "the read after the jump is staged"
    );
    let clock = fx.scripted().expect("scripted");
    assert_eq!(clock.peek_wall_ms(), (NOW + 4 * 3600) * 1000);
    assert_eq!(fx.wall_ms(), (NOW + 4 * 3600) * 1000, "every read after it");
    assert_eq!(fx.wall_s(), NOW + 4 * 3600);
}

/// A RUNNING fixed clock keeps pace with the monotonic clock: it never reads
/// behind its fix, never ahead of the real time since `set_running`, and
/// never backwards.
#[test]
fn a_running_scripted_clock_keeps_pace_with_the_monotonic_clock() {
    let mut fx = Fx::os();
    let clock = fx.scripted().expect("scripted");
    clock.set_at_ms(NOW * 1000);
    let began = Instant::now();
    clock.set_running();
    let mut last = NOW * 1000;
    for _ in 0..3 {
        let read = fx.wall_ms();
        let bound = i64::try_from(began.elapsed().as_millis()).expect("ms");
        assert!(read >= last, "{read} went back from {last}");
        assert!(read - NOW * 1000 <= bound, "{read} ran ahead of {bound} ms");
        last = read;
    }
}

/// A STANDING MONOTONIC CLOCK READS NO REAL TIME: across a real sleep every
/// read, a peek's too, is the one instant, so a test that ages the policy's
/// instants itself is aged by no slow step of a loaded machine. NEGATIVE
/// CONTROL: unstood, the same sleep moves it.
#[test]
fn a_standing_monotonic_clock_reads_no_real_time() {
    let mut fx = Fx::os();
    fx.scripted().expect("scripted").set_at_ms(NOW * 1000);
    let runs = fx.mono();
    std::thread::sleep(Duration::from_millis(2));
    assert!(fx.mono() > runs, "unstood, it is the system's");
    fx.scripted().expect("scripted").stand_mono();
    let stands = fx.mono();
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!((fx.mono(), fx.peek().0), (stands, stands));
    assert_eq!(fx.wall_ms(), NOW * 1000);
}

/// UNFIXED, the scripted clock is the system's: a session whose test stages
/// nothing reads what the shipped loop reads, on every clock of the door.
#[test]
fn an_unfixed_scripted_clock_reads_the_systems() {
    let mut door = Fx::os();
    door.scripted().expect("scripted");
    let before = super::super::limit::unix_now_ms();
    let (wall, stamp) = (door.wall_ms(), door.stamp_ms());
    let after = super::super::limit::unix_now_ms();
    assert!(
        (before..=after).contains(&wall),
        "{before} <= {wall} <= {after}"
    );
    assert!(
        (before..=after).contains(&stamp),
        "{before} <= {stamp} <= {after}"
    );
    let s_before = super::super::limit::unix_now();
    let secs = door.wall_s();
    let s_after = super::super::limit::unix_now();
    assert!(
        (s_before..=s_after).contains(&secs),
        "{s_before} <= {secs} <= {s_after}"
    );
    let t0 = Instant::now();
    let t1 = door.mono();
    assert!(t1 >= t0, "the monotonic clock is the system's");
}

/// THE STAMPS ARE THE SYSTEM'S under a fixed clock, as they were: a journal
/// row's `t` never read the test's fixed clock, and reading it would consume
/// its staged jump at a read the loop did not make.
#[test]
fn stamps_read_the_system_clock_and_never_consume_a_staged_jump() {
    let mut fx = Fx::os();
    let clock = fx.scripted().expect("scripted");
    clock.set_at_ms(NOW * 1000);
    clock.set_jump(4 * 3600);
    let before = super::super::journal::unix_ms();
    let stamp = fx.stamp_ms();
    let after = super::super::journal::unix_ms();
    assert!(
        (before..=after).contains(&stamp),
        "{before} <= {stamp} <= {after}"
    );
    assert_eq!(fx.wall_ms(), NOW * 1000, "the stamps left the jump staged");
    assert_eq!(fx.wall_ms(), (NOW + 4 * 3600) * 1000);
}

/// VIRTUAL CLOCKS move only by sleeps: no real time passes in a sleep, and
/// the monotonic clock, the wall clock and the stamps all advance by exactly
/// what was slept. A fixed clock is refused over them.
#[test]
fn virtual_clocks_move_only_by_sleeps() {
    let origin = Instant::now();
    let clock = Arc::new(VirtualClock::new(origin, NOW * 1000));
    let mut fx = Fx::over(ClockSource::Virtual(Arc::clone(&clock)));
    assert_eq!(fx.mono(), origin);
    assert_eq!(fx.wall_ms(), NOW * 1000);
    let began = Instant::now();
    fx.sleep(Duration::from_secs(3600));
    fx.sleep(Duration::from_millis(1500));
    assert!(
        began.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "an hour's virtual sleep took real time"
    );
    let slept = Duration::from_millis(3_601_500);
    assert_eq!(clock.passed(), slept);
    assert_eq!(fx.mono(), origin + slept);
    assert_eq!(fx.since(origin), slept);
    assert_eq!(fx.wall_ms(), NOW * 1000 + 3_601_500);
    assert_eq!(fx.wall_s(), NOW + 3601);
    assert_eq!(fx.stamp_ms(), NOW * 1000 + 3_601_500);
    assert!(fx.scripted().is_none(), "no fixed clock over virtual ones");
}

/// THE TEST CLOCKS COUNT THE MACHINE'S SLEEP ONLY WHERE A TEST COUNTS
/// ([`Fx::asleep`], [`Fx::last_sleep`]): uncounted — every test before the
/// count — a loop over them reads a sleep by its clocks alone, as on a
/// machine that keeps no count; counted, a sleep they stage is counted, on
/// [`TEST_BOOT`] or the boot the test names, and recorded as the last, and a
/// step that is no sleep (the fixed clock set on, a sleep of the loop's own
/// through the door) is time awake.
#[test]
fn the_test_clocks_count_a_sleep_only_where_a_test_counts() {
    let secs = Duration::from_secs;
    let mut fx = Fx::os();
    let clock = fx.scripted_handle().expect("scripted");
    clock.set_at_ms(NOW * 1000);
    clock.slept(60);
    assert_eq!(fx.wall_ms(), (NOW + 60) * 1000);
    assert_eq!((fx.asleep(), fx.last_sleep()), (None, None), "uncounted");
    clock.count_sleeps(secs(100));
    clock.set_at_ms((NOW + 600) * 1000);
    assert_eq!(fx.asleep().map(|c| c.asleep), Some(secs(100)), "a step");
    // The count moves as the wall clock's reads show the sleep.
    clock.slept(1800);
    assert_eq!(fx.asleep().map(|c| c.asleep), Some(secs(100)));
    assert_eq!(fx.wall_ms(), (NOW + 2460) * 1000);
    let count = fx.asleep().expect("counted");
    assert_eq!(count.asleep, secs(1900));
    assert_eq!(count.boot.map(|b| b.id()), Some(TEST_BOOT));
    assert_eq!(
        fx.last_sleep(),
        Some(((NOW + 660) * 1000, (NOW + 2460) * 1000))
    );
    clock.set_boot(OTHER_TEST_BOOT);
    assert_eq!(
        fx.asleep().and_then(|c| c.boot).map(|b| b.id()),
        Some(OTHER_TEST_BOOT)
    );

    let origin = Instant::now();
    let virt = Arc::new(VirtualClock::new(origin, NOW * 1000));
    let fx = Fx::over(ClockSource::Virtual(Arc::clone(&virt)));
    virt.sleep_mac(secs(600));
    assert_eq!((fx.mono(), fx.wall_ms()), (origin, (NOW + 600) * 1000));
    assert_eq!((fx.asleep(), fx.last_sleep()), (None, None), "uncounted");
    virt.count_sleeps();
    fx.sleep(secs(60));
    virt.sleep_mac(secs(1800));
    assert_eq!(
        fx.mono(),
        origin + secs(60),
        "the sleep stops the awake clock"
    );
    assert_eq!(fx.asleep().map(|c| c.asleep), Some(secs(2400)));
    assert_eq!(
        fx.last_sleep(),
        Some(((NOW + 660) * 1000, (NOW + 2460) * 1000))
    );
}

/// `since` is `elapsed`: zero for an instant ahead of the clock, never a
/// panic.
#[test]
fn since_saturates_like_elapsed() {
    let fx = Fx::os();
    let ahead = Instant::now() + Duration::from_secs(60);
    assert_eq!(fx.since(ahead), Duration::ZERO);
    assert_eq!(ahead.elapsed(), Duration::ZERO);
}

// -- the loop's own derived doors ------------------------------------------------

/// A zone table standing in for `date`.
fn table_zone(zone: &str, unix: i64) -> Option<i64> {
    (zone == "Pacific/Kiritimati").then_some(50_400 + unix % 2)
}

/// A local offset standing in for `date`.
fn table_local(unix: i64) -> Option<i64> {
    (unix > 0).then_some(-25_200)
}

/// The round trips of a finished tape at `door`: request and reply.
fn taped_at(
    read: &crate::supervise::tape::recover::Recovered,
    door: &str,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    read.items
        .iter()
        .filter_map(|i| match i.record() {
            Some(astream_effects::EffectRecord::Exchange {
                door: d,
                request,
                reply,
            }) if d == door => Some((request.clone(), reply.clone())),
            _ => None,
        })
        .collect()
}

/// THE LOOP'S DERIVED DOORS: a zone's offset and the local offset at an
/// instant each come back
/// through a recording door exactly as the live reader answers, and each is
/// one round trip at its own door — the question out, the answer back,
/// which reads back as the value the loop was handed. Live, nothing is
/// recorded and the reader answers alone.
#[test]
fn the_loops_derived_doors_answer_live_and_tape_each_look() {
    use crate::supervise::tape::testing::MemoryTape;

    let live = Fx::over(ClockSource::Scripted(Arc::default()));
    assert_eq!(
        live.zone_offset(table_zone, "Pacific/Kiritimati", 2),
        Some(50_400)
    );
    assert!(live.tape().is_none());

    let tape = MemoryTape::over(&live);
    let fx = &tape.fx;
    assert_eq!(
        fx.zone_offset(table_zone, "Pacific/Kiritimati", 7),
        Some(50_401)
    );
    assert_eq!(fx.zone_offset(table_zone, "Mars/Olympus", 7), None);
    assert_eq!(fx.local_offset_at(table_local, 9), Some(-25_200));
    let read = tape.finish("done");

    let zones = taped_at(&read, doors::PROBE_ZONE_OFFSET);
    assert_eq!(
        zones,
        [
            (b"Pacific/Kiritimati 7".to_vec(), b"50401".to_vec()),
            (b"Mars/Olympus 7".to_vec(), Vec::new()),
        ]
    );
    assert_eq!(
        taped_at(&read, doors::PROBE_LOCAL_OFFSET),
        [(b"9".to_vec(), b"-25200".to_vec())]
    );
}
