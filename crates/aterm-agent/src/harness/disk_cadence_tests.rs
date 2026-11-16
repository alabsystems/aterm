// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const MINUTE: Duration = Duration::from_secs(60);

/// A day of looks a minute apart on a `short` volume whose free figure never moves: the first
/// pass runs at once, each later one only once the doubled wait has passed, and the waits
/// climb 10, 20, 40, 80 minutes to the two-hour cap and stay there. A pass that freed
/// something brings the ten-minute interval back.
#[test]
fn a_short_pass_that_frees_nothing_backs_off_to_its_cap() {
    let t0 = Instant::now();
    let free = Some(3u64 << 30);
    let mut last: Option<Pass> = None;
    let mut runs: Vec<Duration> = Vec::new();
    for minute in 0..24 * 60 {
        let now = t0 + MINUTE * minute;
        if due(&mut last, now, Level::Short, free, false, 0) {
            // Only the first pass frees anything.
            ran(&mut last, now, free, false, Level::Short, runs.is_empty());
            runs.push(now - t0);
        }
    }
    let gaps: Vec<u64> = runs
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs() / 60)
        .collect();
    assert_eq!(&gaps[..5], &[10, 20, 40, 80, 120], "{gaps:?}");
    assert!(gaps[5..].iter().all(|&g| g == 120), "{gaps:?}");
    assert_eq!(runs.len(), 15, "{runs:?}");

    // NEGATIVE CONTROL: a pass that frees something each time keeps the ten-minute interval.
    let mut last: Option<Pass> = None;
    let mut n = 0;
    for minute in 0..24 * 60 {
        let now = t0 + MINUTE * minute;
        if due(&mut last, now, Level::Short, free, false, 0) {
            ran(&mut last, now, free, false, Level::Short, true);
            n += 1;
        }
    }
    assert_eq!(n, 24 * 6);
}

/// While backing off at `short`, a poke calls no pass, and free space falling by less than
/// [`EMERGENCY_DROP`] calls none either; a fall past it, a move to another level, or the wait
/// running out does.
#[test]
fn a_backed_off_short_pass_waits_for_the_disk_to_change() {
    let t0 = Instant::now();
    let free = 3u64 << 30;
    let backed = |every| {
        Some(Pass {
            at: t0,
            free: Some(free),
            fruitless: false,
            level: Level::Short,
            every,
        })
    };
    let mut last = backed(PASS_EVERY * 4);
    let at = t0 + 2 * PASS_EVERY;
    assert!(
        !due(&mut last, at, Level::Short, Some(free), true, 0),
        "a poke"
    );
    let slightly = free - EMERGENCY_DROP / 2;
    assert!(!due(&mut last, at, Level::Short, Some(slightly), false, 0));
    let fell = free - 2 * EMERGENCY_DROP;
    assert!(
        due(&mut last, at, Level::Short, Some(fell), false, 0),
        "fell"
    );
    assert!(
        due(&mut last, at, Level::Emergency, Some(free), false, 0),
        "worse"
    );
    assert!(due(
        &mut last,
        t0 + PASS_EVERY * 4,
        Level::Short,
        Some(free),
        false,
        0
    ));
    // NEGATIVE CONTROL: not backing off, a poke calls a pass at once.
    let mut last = backed(PASS_EVERY);
    assert!(
        due(&mut last, t0 + MINUTE, Level::Short, Some(free), true, 0),
        "a poke"
    );
    assert!(!due(
        &mut last,
        t0 + MINUTE,
        Level::Short,
        Some(free),
        false,
        0
    ));
}

/// The wait doubles across `low` and `short` while nothing is freed, each level's cap holding
/// it, and free space falling by [`EMERGENCY_DROP`] since the last pass brings the base back.
/// NEGATIVE CONTROL: the same pass with free space unmoved doubles.
#[test]
fn the_wait_doubles_across_low_and_short_only_while_nothing_is_freed_and_the_disk_holds() {
    let t0 = Instant::now();
    let at = |level, every| Pass {
        at: t0,
        free: None,
        fruitless: false,
        level,
        every,
    };
    let low = at(Level::Low, LOW_EVERY);
    assert_eq!(every(Level::Low, false, Some(&low), None), LOW_EVERY * 2);
    assert_eq!(every(Level::Low, true, Some(&low), None), LOW_EVERY);
    assert_eq!(
        every(
            Level::Low,
            false,
            Some(&at(Level::Low, LOW_EVERY_MAX)),
            None
        ),
        LOW_EVERY_MAX
    );
    // `low` and `short` are one family: from `low` to `short` the wait keeps doubling, held
    // under the short cap.
    assert_eq!(
        every(Level::Short, false, Some(&low), None),
        SHORT_EVERY_MAX
    );
    assert_eq!(
        every(Level::Low, false, Some(&at(Level::Short, PASS_EVERY)), None),
        LOW_EVERY
    );
    let short = at(Level::Short, PASS_EVERY);
    assert_eq!(
        every(Level::Short, false, Some(&short), None),
        PASS_EVERY * 2
    );
    assert_eq!(
        every(
            Level::Short,
            false,
            Some(&at(Level::Short, SHORT_EVERY_MAX)),
            None
        ),
        SHORT_EVERY_MAX
    );
    assert_eq!(
        every(Level::Emergency, false, Some(&short), None),
        PASS_EVERY
    );
    assert_eq!(every(Level::Short, false, None, None), PASS_EVERY);

    let held = Pass {
        free: Some(8u64 << 30),
        ..at(Level::Short, PASS_EVERY * 4)
    };
    assert_eq!(
        every(
            Level::Short,
            false,
            Some(&held),
            Some((8u64 << 30) - EMERGENCY_DROP)
        ),
        PASS_EVERY,
        "fell"
    );
    assert_eq!(
        every(Level::Short, false, Some(&held), Some(8u64 << 30)),
        PASS_EVERY * 8
    );
}

/// FREE SPACE WANDERING ACROSS THE FLOOR CALLS NO PASS A LOOK: a day of looks a minute apart,
/// the volume alternately 64 MiB under and over its floor (`short`, then `low`), every pass
/// freeing nothing, runs at most 15 passes. NEGATIVE CONTROL: treating a change of level as
/// news (a pass at once on every move) runs one a minute, as the cadence before did.
#[test]
fn free_space_wandering_across_the_floor_backs_off_like_a_steady_level() {
    let t0 = Instant::now();
    let floor = 20u64 << 30;
    let room = 40u64 << 30;
    let mut last: Option<Pass> = None;
    let mut runs = 0;
    for minute in 0..24 * 60 {
        let now = t0 + MINUTE * minute;
        let (level, free) = if minute % 2 == 0 {
            (Level::Short, floor - (64 << 20))
        } else {
            (Level::Low, floor + (64 << 20))
        };
        if due(&mut last, now, level, Some(free), false, room) {
            ran(&mut last, now, Some(free), false, level, false);
            runs += 1;
        }
    }
    assert!(runs <= 15, "{runs} passes in a day");
}
