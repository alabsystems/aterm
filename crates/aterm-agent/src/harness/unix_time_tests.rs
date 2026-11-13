// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn utc_and_lstart_stamps_parse() {
    assert_eq!(parse_utc("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(parse_utc("2026-09-24T04:18:02.796Z"), Some(1_790_223_482));
    assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_utc("garbage"), None);
    assert_eq!(parse_utc("2026-09-24T04:18:02+05:30"), None);
    assert_eq!(parse_utc("2026-09-24T04:18:02.796"), None);
    assert_eq!(parse_utc("2026-09-24T04:18:02Z"), Some(1_790_223_482));
    assert_eq!(
        parse_lstart("Thu Sep 24 23:21:50 2026"),
        parse_utc("2026-09-24T23:21:50Z")
    );
    assert_eq!(
        parse_lstart("Thu Sep  4 03:01:00 2026"),
        parse_utc("2026-09-04T03:01:00Z")
    );
    assert_eq!(parse_lstart("garbage"), None);
}

#[test]
fn a_filetime_reads_as_its_unix_second_floored() {
    const EPOCH: u64 = 116_444_736_000_000_000;
    assert_eq!(filetime_seconds(EPOCH), Some(0));
    assert_eq!(filetime_seconds(EPOCH - 1), None, "before 1970");
    assert_eq!(
        filetime_seconds(EPOCH + 1_790_223_482 * 10_000_000 + 7_960_000),
        Some(1_790_223_482),
        "a fraction floors, as a process birth's second does"
    );
}

/// The start a snapshot names this process by reads back as the second the kernel says it was
/// born in; off Windows a bare count is no start, as before.
#[test]
fn a_start_reads_in_the_spelling_this_host_writes() {
    #[cfg(windows)]
    {
        let pid = std::process::id();
        let start = crate::harness::restart_stamp::kernel_start(pid).expect("this process's start");
        let born = aterm_uds::process::read_process_birth(i32::try_from(pid).unwrap()).unwrap();
        assert_eq!(parse_lstart(&start), Some(born.seconds), "{start}");
    }
    #[cfg(not(windows))]
    assert_eq!(parse_lstart("133000000000000000"), None);
}
