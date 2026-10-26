// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GIB: u64 = 1 << 30;
const MB: f64 = 1e6;
const T0: i64 = 1_792_982_400_000;
const LOOK: i64 = 60_000;
const FLOOR: u64 = 47 * GIB;
const TOTAL: u64 = 460 * GIB;

/// A volume's background: it fills 30 KB/s on its own (logs, Spotlight, caches), and each reading
/// lands up to 20 MB either side of that, as a two-point slope's ends do.
fn background(i: i64) -> f64 {
    let jitter = ((i * 7_919) % 41 - 20) as f64 * MB;
    30_000.0 * (i * 60) as f64 + jitter
}

/// The look at look `i` (a few seconds' drift each), reading `free` less the background.
fn look(f: &mut Fill, i: i64, free: f64) -> Option<Projection> {
    let at = T0 + i * LOOK + (i % 7) * 1_100;
    f.observe(
        at,
        (free - background(i)).max(0.0) as u64,
        Some(TOTAL),
        FLOOR,
    )
}

/// Feeds one look a minute from `free` falling `per_s` bytes a second, `n` looks from look `from`,
/// and returns the standing after each.
fn fill_at(f: &mut Fill, free: u64, per_s: f64, from: i64, n: i64) -> Vec<Option<Projection>> {
    (from..from + n)
        .map(|i| look(f, i, free as f64 - per_s * (i * 60) as f64))
        .collect()
}

fn first_standing(seen: &[Option<Projection>]) -> Option<usize> {
    seen.iter().position(Option::is_some)
}

#[test]
fn a_steady_fill_stands_after_ten_minutes_of_crossings_and_not_before() {
    let mut f = Fill::new();
    assert!(f.cold());
    let seen = fill_at(&mut f, 100 * GIB, 40.0 * MB, 0, 40);
    assert!(!f.cold(), "it read its rates");
    let at = first_standing(&seen).expect("a 40 MB/s fill 53 GiB above its floor stands");
    // The 5- and 10-minute rates read from the eighth look or so, and the streak then needs
    // ten more minutes: never before 17 looks, and within 20.
    assert!((17..=20).contains(&at), "stood at look {at}");
    let p = seen[at].expect("standing");
    assert!((p.rate_bps as f64 - 40.0 * MB).abs() < 2.0 * MB, "{p:?}");
    assert_eq!(p.floor, FLOOR);
    assert_eq!(p.need, p.rate_bps * 7_200);
    assert!(p.eta_ms > 0 && p.eta_ms < HORIZON_MS, "{p:?}");
    assert!(
        seen[at..].iter().all(Option::is_some),
        "it stands while the fill goes on"
    );
    let free = (100 * GIB as i64 - (40.0 * MB * 39.0 * 60.0) as i64) as u64;
    assert!(
        f.crossing(free, FLOOR, Some(TOTAL)).is_some(),
        "and presses the ask"
    );
}

#[test]
fn a_fill_whose_crossing_is_past_the_horizon_never_stands() {
    let mut f = Fill::new();
    // 300 GiB above the floor at 20 MB/s: about 4.5 hours.
    let seen = fill_at(&mut f, 347 * GIB, 20.0 * MB, 0, 90);
    assert_eq!(first_standing(&seen), None);
}

#[test]
fn one_large_file_written_once_stands_nothing() {
    let mut f = Fill::new();
    let seen: Vec<_> = (0..90)
        .map(|i| look(&mut f, i, if i < 30 { 100.0 } else { 70.0 } * GIB as f64))
        .collect();
    assert_eq!(
        first_standing(&seen),
        None,
        "30 GiB written once reads as filling now for five minutes only"
    );
}

#[test]
fn a_fill_that_stops_lapses_within_twenty_minutes_on_a_noisy_volume() {
    let mut f = Fill::new();
    let mut seen = fill_at(&mut f, 100 * GIB, 40.0 * MB, 0, 30);
    let stood = first_standing(&seen).expect("stands");
    let last = 100.0 * GIB as f64 - 40.0 * MB * (29.0 * 60.0);
    for i in 30..70 {
        seen.push(look(&mut f, i, last));
    }
    let lapsed = seen[stood..]
        .iter()
        .position(Option::is_none)
        .map(|i| i + stood)
        .expect("lapses");
    // The 5-minute rate reads the background, under the noise floor, five minutes after the
    // fill stops; the lapse takes ten more.
    assert!((43..=50).contains(&lapsed), "lapsed at look {lapsed}");
}

#[test]
fn a_pass_counted_but_not_measured_stands_nothing() {
    // A local snapshot holds what the pass removed: free space does not rise. Credited with what
    // its removers counted, the volume would read 40 GiB written in a minute.
    let mut f = Fill::new();
    let mut seen = Vec::new();
    for i in 0..70 {
        let free = 80.0 * GIB as f64;
        if i == 30 {
            let before = (free - background(i)) as u64;
            f.credit(40 * GIB, before, before, 60_000);
        }
        seen.push(look(&mut f, i, free));
    }
    assert_eq!(first_standing(&seen), None);
    assert_eq!(credited(40 * GIB, 80 * GIB, 80 * GIB, 0.0, 60_000), 0);
    assert_eq!(
        credited(40 * GIB, 80 * GIB, 120 * GIB, 0.0, 60_000),
        40 * GIB,
        "at most what was counted"
    );
    assert_eq!(
        credited(60 * GIB, 80 * GIB, 120 * GIB, 1e6, 60_000),
        40 * GIB + 60_000_000,
        "the rise plus what the fill took while the pass ran"
    );
}

#[test]
fn what_this_process_freed_is_not_read_as_the_disk_emptying() {
    let mut credited_fill = Fill::new();
    let mut blind = Fill::new();
    let rate = 30.0 * MB;
    let mut freed_total = 0.0;
    let mut seen = Vec::new();
    for i in 0..60 {
        let fell = rate * (i * 60) as f64;
        let free = 120.0 * GIB as f64 + freed_total - fell;
        seen.push(look(&mut credited_fill, i, free));
        let _ = look(&mut blind, i, free);
        if i == 29 {
            // A pass after look 29 frees 60 GiB in a minute; free space rises by that, less the
            // fill meanwhile.
            let before = (free - background(i)) as u64;
            freed_total = 60.0 * GIB as f64;
            let after = (120.0 * GIB as f64 + freed_total
                - rate * (30 * 60) as f64
                - background(30)) as u64;
            credited_fill.credit(60 * GIB, before, after, 60_000);
        }
        if i == 33 {
            let (c, b) = (credited_fill.rates(), blind.rates());
            assert!(
                (c.rate10.expect("read") - rate).abs() < 2.0 * MB,
                "credited, the fill reads on: {c:?}"
            );
            assert!(
                b.rate10.expect("read") < 0.0,
                "uncredited, aterm's own 60 GiB reads as the disk emptying: {b:?}"
            );
        }
    }
    assert!(seen[29].is_some(), "stood before the pass");
    assert!(
        seen[30..].iter().all(Option::is_some),
        "the fill goes on past aterm's own 60 GiB, and so does the projection"
    );
}

#[test]
fn one_crossing_before_a_sleep_does_not_stand_after_it() {
    let mut f = Fill::new();
    let mut seen = Vec::new();
    for i in 0..15 {
        seen.push(look(&mut f, i, 80.0 * GIB as f64));
    }
    // A 20 GiB image written in the last minute before the lid closes: one look projects.
    seen.push(look(&mut f, 15, 60.0 * GIB as f64));
    // Thirty minutes asleep; after it, ten looks with no 10-minute rate, then writes resume.
    for i in 45..56 {
        seen.push(look(&mut f, i, 60.0 * GIB as f64));
    }
    for i in 56..62 {
        seen.push(look(
            &mut f,
            i,
            60.0 * GIB as f64 - 40.0 * MB * ((i - 55) * 60) as f64,
        ));
    }
    assert_eq!(
        first_standing(&seen),
        None,
        "a streak needs ten minutes of its own"
    );
}

#[test]
fn a_cold_ring_projects_nothing_and_a_standing_one_lapses_when_readings_stop() {
    let mut f = Fill::new();
    let seen = fill_at(&mut f, 60 * GIB, 80.0 * MB, 0, 7);
    assert_eq!(first_standing(&seen), None, "under ten minutes of ring");
    assert!(f.cold());
    let mut f = Fill::new();
    let seen = fill_at(&mut f, 100 * GIB, 40.0 * MB, 0, 25);
    assert!(seen.last().copied().flatten().is_some(), "stands");
    // The machine sleeps a day: the ring empties, and with no rate the projection lapses after
    // ten minutes of looks that read none.
    let wake = T0 + 86_400_000;
    let after: Vec<_> = (0..12)
        .map(|i| f.observe(wake + i * LOOK, 40 * GIB, Some(TOTAL), FLOOR))
        .collect();
    assert!(after[0].is_some(), "the first look after waking keeps it");
    assert!(
        f.crossing(40 * GIB, FLOOR, Some(TOTAL)).is_none(),
        "but a look with no rate presses no ask"
    );
    assert!(
        after.last().copied().flatten().is_none(),
        "lapsed: {after:?}"
    );
}

#[test]
fn a_pass_that_frees_the_horizons_fill_presses_the_ask_no_more() {
    let mut f = Fill::new();
    let seen = fill_at(&mut f, 100 * GIB, 10.0 * MB, 0, 25);
    let p = seen
        .last()
        .copied()
        .flatten()
        .expect("10 MB/s from 100 GiB stands");
    assert!(f.crossing(85 * GIB, FLOOR, Some(TOTAL)).is_some());
    // The pass took free space past the floor plus two hours of the fill.
    let after = FLOOR + p.need + GIB;
    assert!(
        f.pressing().is_some(),
        "the pass stays pressed until the projection lapses"
    );
    assert_eq!(
        f.crossing(after, FLOOR, Some(TOTAL)),
        None,
        "the ask is not"
    );
}

#[test]
fn below_the_floor_the_crossing_is_now() {
    let p = Projection::of(40_000_000, 40 * GIB, FLOOR, Some(TOTAL));
    assert_eq!(p.eta_ms, 0);
    let p = Projection::of(40_000_000, 75 * GIB, FLOOR, Some(TOTAL));
    assert_eq!(p.eta_ms, ((28 * GIB) / 40_000_000) as i64 * 1_000);
    assert_eq!(p.need, 288_000_000_000);
    let huge = Projection::of(u64::MAX / 4, 75 * GIB, FLOOR, Some(TOTAL));
    assert_eq!(huge.need, TOTAL, "at most the volume's size");
    assert_eq!(
        Projection::of(36_333_333, 75 * GIB, FLOOR, None).describe(),
        "projected under the 47.0 GiB floor in ~14 min, falling 121.8 GiB/h"
    );
}
