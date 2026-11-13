// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{asleep_since_boot, continuous_ms, continuous_ns, last_sleep_ms};
use std::time::{Duration, Instant};

#[test]
fn the_continuous_clock_counts_at_least_the_awake_time_between_two_reads() {
    let first = continuous_ns().expect("wired on this platform");
    let awake = Instant::now();
    while awake.elapsed() < Duration::from_millis(2) {}
    let between = awake.elapsed();
    let next = continuous_ns().expect("wired on this platform");
    let counted = Duration::from_nanos(next.checked_sub(first).expect("it never runs back"));
    assert!(counted >= between, "{counted:?} {between:?}");
    let ms = continuous_ms().expect("wired on this platform");
    assert!(ms >= next / 1_000_000, "{ms} {next}");
}

#[test]
fn the_sleep_since_boot_reads_and_never_runs_back() {
    let first = asleep_since_boot().expect("wired on this platform");
    let next = asleep_since_boot().expect("wired on this platform");
    assert!(
        next + Duration::from_millis(1) >= first,
        "{first:?} {next:?}"
    );
    let since_boot = Duration::from_millis(continuous_ms().expect("the boot clock"));
    assert!(
        next <= since_boot + Duration::from_millis(1),
        "{next:?} {since_boot:?}"
    );
}

#[test]
fn the_last_sleep_is_in_order() {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let last = last_sleep_ms();
    if let Some((slept, woke)) = last {
        assert!(
            0 < slept && slept < woke && woke <= now_ms + 1000,
            "{slept} {woke} {now_ms}"
        );
    }
    #[cfg(target_vendor = "apple")]
    if last.is_none() {
        let slept =
            super::sysctl_timeval_ms(c"kern.sleeptime").expect("the kernel answers kern.sleeptime");
        let woke =
            super::sysctl_timeval_ms(c"kern.waketime").expect("the kernel answers kern.waketime");
        assert!(slept == 0 || slept >= woke, "{slept} {woke}");
    }
}
