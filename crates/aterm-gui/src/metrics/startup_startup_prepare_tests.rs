// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    ClaimStamp, LOOP_FIRST_CUTS, STARTUP_PREPARE_CUTS, birth_to_main_ns, derive_startup_prepare,
    derive_startup_prepare_of,
};
use std::time::{Duration, Instant, UNIX_EPOCH};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn cuts(entry: Instant) -> [Option<Instant>; 9] {
    [4, 14, 34, 36, 41, 52, 60, 75, 90].map(|n| Some(entry + ms(n)))
}

#[test]
fn a_missing_or_out_of_order_boundary_leaves_the_split_invalid() {
    let entry = Instant::now();
    let ready = Some(entry + ms(110));
    for missing in 0..9 {
        let mut gone = cuts(entry);
        gone[missing] = None;
        assert_eq!(derive_startup_prepare(Some(entry), gone, ready), None);
    }
    assert_eq!(derive_startup_prepare(None, cuts(entry), ready), None);
    assert_eq!(derive_startup_prepare(Some(entry), cuts(entry), None), None);
    let mut swapped = cuts(entry);
    swapped.swap(5, 6);
    assert_eq!(derive_startup_prepare(Some(entry), swapped, ready), None);
    assert_eq!(
        derive_startup_prepare(Some(entry), cuts(entry), Some(entry + ms(80))),
        None
    );
}

fn stamped(entry: Instant, order: [ClaimStamp; 9]) -> impl Fn(ClaimStamp) -> Option<Instant> {
    let at: Vec<(ClaimStamp, Option<Instant>)> = order.into_iter().zip(cuts(entry)).collect();
    move |stamp| {
        at.iter()
            .find(|(cut, _)| *cut == stamp)
            .and_then(|(_, t)| *t)
    }
}

#[test]
fn either_boot_order_is_split_under_the_same_names() {
    let entry = Instant::now();
    let ready = Some(entry + ms(110));
    let of = |order| derive_startup_prepare_of(Some(entry), stamped(entry, order), ready);
    assert_eq!(
        of(STARTUP_PREPARE_CUTS),
        Some([4, 10, 20, 2, 5, 11, 8, 15, 15, 20].map(|n| n * 1_000_000)),
        "session 0 first: journal 20, session0 2, event_loop 5"
    );
    assert_eq!(
        of(LOOP_FIRST_CUTS),
        Some([4, 10, 2, 5, 20, 11, 8, 15, 15, 20].map(|n| n * 1_000_000)),
        "the loop first: its 20 ms build is still event_loop's"
    );
    let mut neither = STARTUP_PREPARE_CUTS;
    neither.swap(3, 4);
    assert_eq!(of(neither), None);
}

fn start(boot: u64) -> aterm_uds::process::ProcessStart {
    aterm_uds::process::ProcessStart {
        boot: aterm_uds::process::BootClock(boot),
        wall: Some(aterm_uds::process::ProcessBirth {
            seconds: 1_792_118_934,
            microseconds: 927_250,
        }),
    }
}

const APPLE_SILICON: Option<(u32, u32)> = Some((125, 3));

#[test]
fn birth_to_main_is_read_on_the_boot_clock_through_a_wall_step() {
    let born = start(1 << 30);
    let main_boot = Some(aterm_uds::process::BootClock((1 << 30) + 120_000));
    let stepped = UNIX_EPOCH
        + Duration::from_secs(1_792_118_934 + 3_600)
        + Duration::from_micros(927_250 + 5_000);
    assert_eq!(
        birth_to_main_ns(born, main_boot, stepped, APPLE_SILICON),
        Some(5_000_000)
    );
    assert_eq!(
        birth_to_main_ns(born, None, stepped, APPLE_SILICON),
        Some(3_600_005_000_000),
        "the wall clock alone reads the step"
    );
    let early = Some(aterm_uds::process::BootClock((1 << 30) - 1));
    assert_eq!(birth_to_main_ns(born, early, stepped, APPLE_SILICON), None);
}

#[test]
fn birth_to_main_falls_back_to_the_kernel_birth_to_the_microsecond() {
    let main = UNIX_EPOCH + Duration::from_secs(1_792_118_936) + Duration::from_micros(857_000);
    assert_eq!(
        birth_to_main_ns(start(0), None, main, APPLE_SILICON),
        Some(1_929_750_000)
    );
    let boot = Some(aterm_uds::process::BootClock(5));
    assert_eq!(
        birth_to_main_ns(start(0), boot, main, None),
        Some(1_929_750_000),
        "no timebase: the wall clock"
    );
    let before = UNIX_EPOCH + Duration::from_secs(1_792_118_934);
    assert_eq!(
        birth_to_main_ns(start(0), None, before, APPLE_SILICON),
        None
    );
}

/// On this machine: the kernel's start record of this very process orders
/// before a boot-clock reading taken now, and the timebase turns ticks
/// into the nanoseconds `Instant` counts.
#[cfg(target_vendor = "apple")]
#[test]
fn the_boot_clock_reads_in_nanoseconds_through_this_machines_timebase() {
    let pid = i32::try_from(std::process::id()).expect("a pid");
    let born = aterm_uds::process::read_process_start(pid).expect("our own start record");
    let now = aterm_uds::process::boot_clock_now();
    assert!(
        birth_to_main_ns(
            born,
            now,
            std::time::SystemTime::now(),
            super::mach_timebase()
        )
        .is_some(),
        "born before now on the boot clock"
    );
    if aterm_uds::process::translation() == aterm_uds::process::Translation::Translated {
        assert_eq!(now, None, "a translated process reads no boot clock");
        println!(
            "NOT RUN under Rosetta: metrics::startup::startup_prepare_tests::\
             the_boot_clock_reads_in_nanoseconds_through_this_machines_timebase: a translated \
             process reads no boot clock"
        );
        return;
    }
    let boot = || aterm_uds::process::boot_clock_now().expect("a native boot clock");
    let (outer0, first, inner0) = (Instant::now(), boot(), Instant::now());
    std::thread::sleep(ms(20));
    let (inner1, second, outer1) = (Instant::now(), boot(), Instant::now());
    let at_first = aterm_uds::process::ProcessStart {
        boot: first,
        wall: None,
    };
    let read = birth_to_main_ns(
        at_first,
        Some(second),
        std::time::SystemTime::now(),
        super::mach_timebase(),
    )
    .expect("a later reading orders");
    let inner = u64::try_from((inner1 - inner0).as_nanos()).expect("fits");
    let outer = u64::try_from((outer1 - outer0).as_nanos()).expect("fits");
    assert!(
        (inner.saturating_sub(1_000)..=outer + 1_000).contains(&read),
        "{read} ns outside [{inner}, {outer}]"
    );
}
