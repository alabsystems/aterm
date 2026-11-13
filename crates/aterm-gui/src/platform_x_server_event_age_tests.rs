// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::ffi::OsStr;

use super::{x_event_max_age_ms, x_server_event_age_ns};

const MS: u64 = 1_000_000;
const LOCAL: u32 = 60_000;
const FOREIGN: u32 = 2_000;

#[test]
fn age_is_now_minus_the_stamp_with_the_sub_millisecond_remainder_kept() {
    assert_eq!(
        x_server_event_age_ns(1_000, 1_004 * MS + MS / 4, LOCAL),
        Some(4 * MS + MS / 4)
    );
    assert_eq!(
        x_server_event_age_ns(1_000, 1_000 * MS + 600_000, LOCAL),
        Some(600_000)
    );
}

#[test]
fn the_32_bit_server_clock_wraps_about_every_49_days() {
    let now_ns = ((1_u64 << 32) + 3) * MS;
    assert_eq!(
        x_server_event_age_ns(u32::MAX - 1, now_ns, LOCAL),
        Some(5 * MS)
    );
    assert_eq!(
        x_server_event_age_ns(u32::MAX - 2_699, now_ns, LOCAL),
        Some(2_703 * MS)
    );
}

#[test]
fn a_stamp_just_in_the_future_is_granularity_and_reads_as_no_queueing() {
    assert_eq!(x_server_event_age_ns(1_002, 1_000 * MS, LOCAL), Some(0));
    assert_eq!(x_server_event_age_ns(1_003, 1_000 * MS, LOCAL), None);
}

#[test]
fn a_local_server_prices_a_multi_second_park() {
    let now_ns = 100_000 * MS;
    assert_eq!(
        x_server_event_age_ns(100_000 - 2_722, now_ns, LOCAL),
        Some(2_722 * MS)
    );
    assert_eq!(
        x_server_event_age_ns(100_000 - 60_000, now_ns, LOCAL),
        Some(60_000 * MS)
    );
    assert_eq!(x_server_event_age_ns(100_000 - 60_001, now_ns, LOCAL), None);
    assert_eq!(
        x_server_event_age_ns(100_000 - 2_722, now_ns, FOREIGN),
        None
    );
}

#[test]
fn a_foreign_server_keeps_the_two_second_window() {
    assert_eq!(x_server_event_age_ns(1_000, 3_001 * MS, FOREIGN), None);
    assert_eq!(
        x_server_event_age_ns(1_000, 3_000 * MS, FOREIGN),
        Some(2_000 * MS)
    );
    assert_eq!(x_server_event_age_ns(500_000, 1_000 * MS, FOREIGN), None);
    assert_eq!(x_server_event_age_ns(500_000, 1_000 * MS, LOCAL), None);
}

#[test]
fn current_time_is_not_a_stamp() {
    assert_eq!(x_server_event_age_ns(0, 30_000 * MS, LOCAL), None);
    assert_eq!(
        x_server_event_age_ns(1, 30_000 * MS, LOCAL),
        Some(29_999 * MS)
    );
}

#[test]
fn the_cap_follows_the_display_the_process_connects_to() {
    for local in [":0", ":1", ":1.0", "unix:0", "/tmp/.X11-unix/X1"] {
        assert_eq!(
            x_event_max_age_ms(Some(OsStr::new(local))),
            LOCAL,
            "{local}"
        );
    }
    for foreign in ["localhost:10.0", "host.example:0", "10.0.0.2:0", ""] {
        assert_eq!(
            x_event_max_age_ms(Some(OsStr::new(foreign))),
            FOREIGN,
            "{foreign}"
        );
    }
    assert_eq!(x_event_max_age_ms(None), FOREIGN);
}
