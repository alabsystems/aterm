// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: hang a socket client up the way its PROCESS hangs up, and join
//! what served it with a hang detector.
//!
//! `drop` closes a descriptor, and a socket closes only with its LAST one. In
//! this lib-test process sibling tests fork all the time (forkpty, `pre_exec`
//! commands, `ParkedChild`, raw `fork`), and each child holds a copy of every
//! descriptor until it execs: a window measured past ten seconds at load ~150
//! for a child demoted to the Darwin background band before its exec (macOS
//! 26.6 does not honor `FD_CLOFORK`). Worse, std on macOS marks a socketpair,
//! a `socket` and an `accept`ed stream close-on-exec only AFTER creating it, so
//! a fork in that instant hands over a copy that outlives the exec too. While a
//! copy lives, the server end of a dropped client reads no EOF and its writes
//! still succeed — into the 8 KiB buffer, then they block (measured on macOS
//! 26.6). A push loop that waited for a write to fail therefore pushed every
//! teardown frame into a dead socket, went quiet, and parked on its 250 ms
//! tick for good: a 25-minute hang of a `sessions`-stream subscription test on
//! 2026-09-30. A relay's client-to-child pump, waiting for that EOF, parks the
//! same way.
//!
//! [`hang_up`] acts on the socket instead of the descriptor. A real `aterm ctl`
//! client is a process of its own, whose exit closes the last descriptor; this
//! is how a test stands in for that exit. It does NOT stand in for a probe that
//! needs the last close itself — the lanes' `POLLHUP` hangup check
//! (`aterm_uds::hangup`) — which a test waits for as a fact instead
//! (`control_hangup_tests`).

use std::thread::JoinHandle;

use aterm_uds::CtlStream;

/// HANG UP a test's client for the whole socket, not for one descriptor of
/// it: the server end reads EOF at once whatever copies of the client end
/// survive, and its writes are discarded without blocking. The production
/// rule ([`aterm_uds::hangup::hang_up`]: each half on its own, because Darwin
/// refuses a both-ways `shutdown` once the peer half-closed, and then shuts
/// nothing), and then this descriptor's close.
pub(crate) fn hang_up(client: CtlStream) {
    aterm_uds::hangup::hang_up(&client);
}

/// Join a thread that must END once its client hung up — a push loop, a
/// relay, a parked verb — with a HANG DETECTOR: one that never ends fails the
/// test at [`aterm_spec::wait::MUST_HAPPEN`] instead of parking the suite in
/// an unbounded `join`. Its value comes back; a panic inside it is re-raised
/// as itself.
#[track_caller]
pub(crate) fn join_after_hang_up<T>(join: JoinHandle<T>, what: &str) -> T {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
        join.is_finished().then_some(())
    });
    match join.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
