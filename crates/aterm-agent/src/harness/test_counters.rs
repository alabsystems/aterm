// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The work a build with debug assertions counts on each thread, for the tests of other crates
//! that hold a read to the work it does (`aterm-gui`'s `work_counts`). A release build has none.

use std::cell::Cell;

use super::looks::Input;

thread_local! {
    static PROFILE_DECODES: Cell<usize> = const { Cell::new(0) };
    static GATHERS: Cell<[usize; Input::ALL.len()]> = const { Cell::new([0; Input::ALL.len()]) };
}

/// One decode of a Claude Code profile's bytes on this thread (`profile::decode`).
pub(crate) fn count_profile_decode() {
    PROFILE_DECODES.with(|n| n.set(n.get() + 1));
}

/// This thread's decodes of a Claude Code profile's bytes since the last take, zeroed.
pub fn take_profile_decodes() -> usize {
    PROFILE_DECODES.with(|n| n.replace(0))
}

/// One gather of `input` on this thread: a footer read that read it because it moved since the
/// read before it (`footer::read_pid`'s row, transcript, subagents and git reads).
pub(crate) fn count_gather(input: Input) {
    GATHERS.with(|n| {
        let mut counts = n.get();
        counts[input.index()] += 1;
        n.set(counts);
    });
}

/// This thread's gathers since the last take, by [`Input::index`], zeroed.
pub fn take_gathers() -> [usize; Input::ALL.len()] {
    GATHERS.with(|n| n.replace([0; Input::ALL.len()]))
}
