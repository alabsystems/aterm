// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::scan_path_users_of;
use crate::progress::ticks_here;

/// No process has this id, so a tick for it is the per-process one alone.
const NONE: u32 = 2_000_000_000;

/// NEGATIVE CONTROL: a process the scan skips is not read, so it does not tick.
#[test]
fn a_scan_ticks_for_each_process_it_reads_and_none_it_skips() {
    let dirs = [std::env::temp_dir()];
    let before = ticks_here();
    scan_path_users_of(&[NONE, NONE], &dirs, &[]).expect("scanned");
    assert!(ticks_here() - before >= 2);
    let me = std::process::id();
    let before = ticks_here();
    scan_path_users_of(&[me], &dirs, &[me]).expect("scanned");
    assert_eq!(ticks_here(), before);
}

/// One process can hold thousands of regions, each a question to the kernel.
#[cfg(target_vendor = "apple")]
#[test]
fn a_scan_ticks_for_each_mapped_region_of_a_process() {
    let dirs = [std::env::temp_dir().join("aterm-progress-nothing-maps-this")];
    let before = ticks_here();
    scan_path_users_of(&[std::process::id()], &dirs, &[]).expect("scanned");
    assert!(ticks_here() - before > 1, "more than the process's own");
}
