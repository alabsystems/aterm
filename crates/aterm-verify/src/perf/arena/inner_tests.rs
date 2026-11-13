// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_benchmarks_wait_for_the_asked_grid() {
    let mut seen = ["17 49", "17 49", "40 120"].into_iter();
    let mut reads = 0;
    await_grid("40 120", GRID_HANG, || {
        reads += 1;
        Ok(seen.next().unwrap_or("24 80").to_string())
    })
    .expect("the third read is the asked grid");
    assert_eq!(reads, 3);
}

#[test]
fn a_grid_never_taken_ends_the_trial_naming_the_grid_it_had() {
    let e = await_grid("40 120", Duration::ZERO, || Ok("17 49".to_string()))
        .expect_err("past the deadline");
    assert!(e.contains("'17 49'") && e.contains("'40 120'"), "{e}");
    assert!(e.ends_with("after 0s"), "the wait it was given: {e}");
    let e = await_grid("40 120", Duration::ZERO, || Err("no tty".to_string()))
        .expect_err("a failed read");
    assert_eq!(e, "no tty");
}
