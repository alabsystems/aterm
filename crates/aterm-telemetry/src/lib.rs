// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-telemetry` — ONE ROW PER ACTION, AND THE LOCAL LOG THAT KEEPS THEM (Continuum TM-0:
//! trust `docs/design/2026-11-02-continuum.md` §6.10 and §11 Wave 0).
//!
//! Every action aterm watches or admits writes one versioned [`Row`] ([`row::SCHEMA`] 1): its
//! key (a digest of what it ran and where, never the command line), its kind, the host and the
//! digest of the host's facts ([`facts::HostFacts`]), wall time, CPU, peak memory, admission
//! wait, cache tier, outcome and stop cause. A field aterm cannot know says so and names the
//! source that would supply it (`cache=?targo`); none is guessed.
//!
//! Rows go through a [`Sink`] (a thread of its own behind a bounded queue, so no session waits
//! on the disk) to the [`Log`]: `<state>/telemetry/rows.log`, rotated at 4 MiB into at most four
//! files, every line checksummed so a crash mid-write costs one torn line, never a wrong one.
//! [`log::read`] and [`summary`] are the reader `aterm ctl telemetry` answers with.
//!
//! The sources are what aterm already measures. In the window: a tab's commands (`aterm-gui`'s
//! status sweep closes each OSC 133 block once, and the shell's own CPU accounting brackets it),
//! an agent's tool calls (the jobs engine's `agent-call` jobs) and worker spawns. In the
//! claimant: each admission decided in the account's own capacity ledger (`aterm-capacity`
//! writes its `kind=admission` row as the decision is made, its `wall_ms` and `wait_ms` both the
//! admission wait, its CPU and peak `na`). Where its claim ends: each build or test that claim
//! held (atpkg's build admission, `kind=build` and `kind=test`). In `aterm ctl`: each worker a
//! fleet spawn sent to another Mac (`kind=spawn`, `on=` that Mac's node id), and its board row's
//! close with that Mac's record (`kind=task`). A process with no [`Sink`] writes through the one
//! direct writer, [`direct`].

#![warn(missing_docs)]

pub mod direct;
pub mod facts;
pub mod log;
pub mod row;
pub mod sink;
pub mod summary;

#[cfg(test)]
mod tests;

pub use facts::HostFacts;
pub use log::{Limits, Log};
pub use row::{Digest, Fig, Kind, Meter, Outcome, Row, Stop};
pub use sink::{Sink, emit, global, install};

/// The wall clock, unix ms (0 where the clock reads before 1970).
#[must_use]
pub fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

/// Start this process's writer on the log under its state directory, with the host's facts
/// read now: the [`HostFacts`] every row it writes should name. `None` where the process has
/// no state directory, the writer could not start, or one is already installed.
#[must_use]
pub fn start_default() -> Option<HostFacts> {
    let dir = log::default_dir()?;
    let facts = HostFacts::read();
    let log = Log::new(dir, Limits::DEFAULT, facts.clone());
    let sink = Sink::start(log).ok()?;
    install(sink).ok()?;
    Some(facts)
}
