// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

// ========================================================================
// Direct tests for unsafe get_unchecked / get_unchecked_mut
// ========================================================================

#[test]
fn get_unchecked_valid_read() {
    let (_pages, mut row) = make_row(10);
    row.write_char(3, 'Q');

    // SAFETY: col 3 < cols() == 10
    let cell = unsafe { row.get_unchecked(3) };
    assert_eq!(cell.char(), 'Q');
}

#[test]
fn get_unchecked_first_and_last_col() {
    let (_pages, mut row) = make_row(5);
    row.write_char(0, 'A');
    row.write_char(4, 'E');

    // SAFETY: cols 0 and 4 are both < cols() == 5
    let first = unsafe { row.get_unchecked(0) };
    let last = unsafe { row.get_unchecked(4) };
    assert_eq!(first.char(), 'A');
    assert_eq!(last.char(), 'E');
}

#[test]
fn get_unchecked_mut_write_and_read() {
    let (_pages, mut row) = make_row(10);

    // SAFETY: col 7 < cols() == 10
    let cell = unsafe { row.get_unchecked_mut(7) };
    cell.set_char('Z');

    assert_eq!(row.get(7).unwrap().char(), 'Z');
}

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
