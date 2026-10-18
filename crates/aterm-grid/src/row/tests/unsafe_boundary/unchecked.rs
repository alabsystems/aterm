// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

// Direct tests for unsafe get_unchecked / get_unchecked_mut

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "assertion")]
fn get_unchecked_out_of_bounds_panics_in_debug() {
    let (_pages, row) = make_row(10);
    // SAFETY: intentionally violating precondition to test debug_assert
    let _ = unsafe { row.get_unchecked(10) };
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "assertion")]
fn get_unchecked_mut_out_of_bounds_panics_in_debug() {
    let (_pages, mut row) = make_row(10);
    // SAFETY: intentionally violating precondition to test debug_assert
    let _ = unsafe { row.get_unchecked_mut(10) };
}
