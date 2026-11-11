// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PathLink, PathStatus};

/// A path's link is the first kind it uses in the asked order, and a path that uses none
/// of them (a tunnel, a bridge) is `Other`.
/// NEGATIVE CONTROL: answering `Other` whatever the path uses fails the Wi-Fi row.
#[test]
fn a_paths_link_is_the_first_asked_kind_it_uses() {
    assert_eq!(PathLink::of(|kind| kind == 1), PathLink::Wifi);
    assert_eq!(PathLink::of(|kind| kind == 3), PathLink::Wired);
    assert_eq!(PathLink::of(|kind| kind == 2), PathLink::Cellular);
    assert_eq!(PathLink::of(|kind| kind == 4), PathLink::Loopback);
    assert_eq!(PathLink::of(|_| false), PathLink::Other);
    assert_eq!(PathLink::of(|kind| kind == 1 || kind == 2), PathLink::Wifi);
}

/// The framework opens and every function a monitor calls resolves; a started monitor
/// reads this Mac's network path, which a test leaves alone.
#[test]
fn the_framework_opens_and_every_function_resolves() {
    assert!(super::Network::open().is_some());
}

/// Each `nw_path_status_t` the header names reads as its own status, and every other
/// value as invalid.
#[test]
fn each_raw_status_reads_as_the_one_the_header_names() {
    let table = [
        (0, PathStatus::Invalid),
        (1, PathStatus::Satisfied),
        (2, PathStatus::Unsatisfied),
        (3, PathStatus::Satisfiable),
        (4, PathStatus::Invalid),
        (-1, PathStatus::Invalid),
        (i32::MAX, PathStatus::Invalid),
        (i32::MIN, PathStatus::Invalid),
    ];
    for (raw, status) in table {
        assert_eq!(PathStatus::from_raw(raw), status, "raw {raw}");
    }
}
