// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::PathChanges;

/// The monitor's first reading is the path in use as it started, and no change; every later
/// reading is one.
/// NEGATIVE CONTROL: a first reading taken as a change would clear every seat's advance at
/// each generation's start; it reads `false` here.
#[test]
fn every_path_reading_after_the_first_is_a_change() {
    let changes = PathChanges::default();
    assert!(!changes.read(), "the first reading is the path in use");
    assert!(changes.read());
    assert!(changes.read());
}
