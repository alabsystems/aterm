// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What Commons repair did in this process, counted: its rounds by direction, its serving
//! loops' ticks, and what its transports read and started. Each count only rises, from the
//! process's start, whichever runtime or transport did the work; a reader takes two readings and
//! subtracts. They decide nothing: no schedule reads them.

use std::sync::atomic::{AtomicU64, Ordering};

/// One counted event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// A round this side dialled ended, whatever it came to (`Transport::finish`).
    RoundOut,
    /// A round a peer dialled in ended, whatever it came to.
    RoundIn,
    /// One of those rounds completed with its peer authenticated and moved nothing.
    RoundQuiet,
    /// A serving loop ran a tick (`super::serve_paced`).
    OwnerTick,
    /// A transport read a discovery datagram that its multicast socket's checks passed.
    Beacon,
    /// A transport's turn took what local appends announced since the turn before
    /// (`super::announce`): one count, however many appends.
    Announce,
    /// An exchange started its `db-exchange-deadline` thread (`crate::network::Deadline`).
    DeadlineSpawn,
}

const EVENTS: usize = 7;

static COUNTS: [AtomicU64; EVENTS] = [const { AtomicU64::new(0) }; EVENTS];

#[cfg(test)]
thread_local! {
    /// The calling thread's own counts ([`cadence_here`]).
    static HERE: [std::cell::Cell<u64>; EVENTS] = const { [const { std::cell::Cell::new(0) }; EVENTS] };
}

/// Count `event` once.
pub(crate) fn count(event: Event) {
    COUNTS[event as usize].fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    HERE.with(|here| {
        let cell = &here[event as usize];
        cell.set(cell.get() + 1);
    });
}

/// Commons repair's counts in this process since it started ([`cadence`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cadence {
    /// Rounds this side dialled that ended, whatever each came to.
    pub rounds_out: u64,
    /// Rounds peers dialled in that ended, whatever each came to.
    pub rounds_in: u64,
    /// Rounds of either direction that completed with their peer authenticated and moved
    /// nothing.
    pub rounds_quiet: u64,
    /// Ticks its serving loops ran.
    pub owner_ticks: u64,
    /// Discovery datagrams its transports read.
    pub beacons: u64,
    /// Turns of its transports that took the announce of local appends: one for the appends
    /// that came since the turn before, however many.
    pub announces: u64,
    /// Fleet book entries' signatures this process checked, by any caller
    /// (`aterm_net::fleet_member::signature_checks`).
    pub signature_checks: u64,
    /// `db-exchange-deadline` threads its exchanges started.
    pub deadline_spawns: u64,
}

impl Cadence {
    fn of(read: impl Fn(Event) -> u64, signature_checks: u64) -> Self {
        Self {
            rounds_out: read(Event::RoundOut),
            rounds_in: read(Event::RoundIn),
            rounds_quiet: read(Event::RoundQuiet),
            owner_ticks: read(Event::OwnerTick),
            beacons: read(Event::Beacon),
            announces: read(Event::Announce),
            signature_checks,
            deadline_spawns: read(Event::DeadlineSpawn),
        }
    }
}

/// The counts now.
#[must_use]
pub fn cadence() -> Cadence {
    Cadence::of(
        |event| COUNTS[event as usize].load(Ordering::Relaxed),
        aterm_net::fleet_member::signature_checks(),
    )
}

/// [`cadence`] of what the calling thread itself did, so a test reads its own counts beside the
/// tests that run with it: a round counts on the thread that takes its end, a tick on the thread
/// that runs the loop, and a deadline thread on the thread that starts it.
#[cfg(test)]
pub(crate) fn cadence_here() -> Cadence {
    Cadence::of(
        |event| HERE.with(|here| here[event as usize].get()),
        aterm_net::fleet_member::signature_checks_here(),
    )
}
