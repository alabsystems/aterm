// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each pass counts once in all, and once under its caller.

use super::*;

/// What this thread's pass counts moved by over `f`, as `[all, keeper, session, moved]`.
fn passes_moved(f: impl FnOnce()) -> [u64; 4] {
    let list = |c: UpkeepCounts| [c.passes, c.passes_keeper, c.passes_session, c.passes_moved];
    let before = list(counts_here());
    f();
    let after = list(counts_here());
    std::array::from_fn(|i| after[i] - before[i])
}

/// A pass counts in all, and under the keeper on the keeper's thread, under the network move's
/// thread on one marked so, and under a session's anywhere else.
///
/// NEGATIVE CONTROL (2026-11-16): `count_pass` asking `MOVED_THREAD` before `in_keeper` failed at
/// "a pass the keeper enters on the moved thread", left `[1, 0, 0, 1]`, right `[1, 1, 0, 0]`.
/// Restored, the file touched.
#[test]
fn each_pass_counts_in_all_and_under_its_caller() {
    assert_eq!(passes_moved(count_pass), [1, 0, 1, 0], "a session's pass");
    assert_eq!(
        passes_moved(|| aterm_uds::trace::keeper(count_pass)),
        [1, 1, 0, 0],
        "the keeper's pass"
    );
    let moved = std::thread::spawn(|| {
        mark_moved_thread();
        let moved = passes_moved(count_pass);
        let keeper = passes_moved(|| aterm_uds::trace::keeper(count_pass));
        (moved, keeper)
    })
    .join()
    .expect("the moved thread");
    assert_eq!(moved.0, [1, 0, 0, 1], "the network move's pass");
    assert_eq!(
        moved.1,
        [1, 1, 0, 0],
        "a pass the keeper enters on the moved thread"
    );
    assert_eq!(
        passes_moved(count_pass),
        [1, 0, 1, 0],
        "a mark is its thread's"
    );
}
