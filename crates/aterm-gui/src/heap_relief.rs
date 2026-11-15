// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The allocator's free pages given back after bursts of work, on Linux (ATERM-KI-1114-33):
//! the short-lived threads of a member round, a repo pass, a control connection or a tab's reader
//! leave their frees in glibc's thread heaps, resident until a release returns them. The node asks
//! on its upkeep ticks ([`relieve`]); the window, whose main thread has no such tick, on a thread of
//! its own ([`start`]).

use aterm_uds::process::ReliefRule;
use std::time::Duration;

/// When a release is owed: once a process stopped growing a floor of 32 MiB past what the last
/// relief release left (one member round of the node added 82 MB and gave back 37), at most once a
/// minute, and at least every ten minutes while it is settled, for what a burst strands as it ends
/// after a release made while its memory was live.
const RULE: ReliefRule = ReliefRule {
    floor: 32 << 20,
    spacing: Duration::from_secs(60),
    sweep: Duration::from_secs(600),
};

/// How often the window's relief thread asks: often enough that a burst's settling shows between
/// two asks, rarely enough that the asks, one `/proc/self/status` read each, cost nothing.
const EVERY: Duration = Duration::from_secs(5);

/// Give the allocator's free pages back where this process owes a release under [`RULE`]
/// (`aterm_uds::process::relieve_heap_if_due`), and log what came back. A unit test runs inside
/// the shared test process, whose heaps are its other tests', so it releases none.
pub(crate) fn relieve() {
    if cfg!(test) {
        return;
    }
    if let Some(relief) = aterm_uds::process::relieve_heap_if_due(RULE) {
        aterm_log::info!(
            "heap relief: {} MB -> {} MB in {} ms",
            relief.before >> 20,
            relief.after >> 20,
            relief.took.as_millis()
        );
    }
}

/// Start the window's relief thread (`heap-relief`), which asks every [`EVERY`] in the background
/// lane. Linux glibc only: elsewhere the process allocator's relief has its own triggers.
pub(crate) fn start() {
    if cfg!(test) || !cfg!(all(target_os = "linux", target_env = "gnu")) {
        return;
    }
    let spawned =
        crate::qos::spawn_lane_thread("heap-relief", crate::qos::Role::Background, || {
            loop {
                std::thread::sleep(EVERY);
                relieve();
            }
        });
    if let Err(error) = spawned {
        aterm_log::warn!("heap relief: the thread did not start: {error}");
    }
}
