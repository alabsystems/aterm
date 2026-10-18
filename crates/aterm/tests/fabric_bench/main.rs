// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC MEASUREMENT: what aterm's use of the astream bus costs, as
//! numbers, so every later speed claim about it is one.
//!
//! It stands up the REAL stack, the way the fabric worlds do — it mounts their
//! harness (`fabric_world/harness.rs`) and boots `harness::World`s: astream's
//! broker opened as aterm opens it (`Broker::open_guarded`, which is
//! `Durability::Strict`, an fsync before every ack), the headless `aterm`
//! this package builds, and the `aterm link serve` bridge that `aterm`
//! launches and supervises. Nothing is stubbed: a message here travels every
//! hop a person's does. It reports, each as `n` with mean and nearest-rank
//! p50/p95/p99 ([`stats`]):
//!
//! 1. **post -> deliver**: one session posts to another on the same node; from
//!    the `post` reply to the row visible in the recipient's inbox (its
//!    `await inbox` answering), split at the moment the record was durable on
//!    the broker.
//! 2. **burst throughput**: N posts back to back to one recipient; when all N
//!    were accepted, durable, visible, and covered by the recipient bridge's
//!    inbox commit, and each message's own post -> visible.
//! 3. **ping-pong**: A posts to B and B replies the moment it sees the message,
//!    back to back (default 200 rounds): the round trip, each reply's wait for
//!    the inbox commit of the delivery it answers, and the share of the window
//!    the bridge's one loop spent blocked on broker replies, counted inside the
//!    bridge by its wait-tally seam (`aterm_link::wait_tally`).
//! 4. **attach time**: a bridge starting with N sessions (default 1, 8, 32),
//!    from its pid file to every session's presence row durable again.
//! 5. **records per delivered message**: every record the broker wrote over
//!    each measured window, hidden commits included, by kind ([`census`]) —
//!    the count the bridge's batching reduces (in the burst, how far depends
//!    on scheduling: the README says which counts compare).
//!
//! How the times are taken, and why they need no clock inside the stack, is
//! [`clock`]; what each phase does is [`run`]. The one count taken inside it is
//! the ping-pong's: how long the bridge's loop waited on the broker, which only
//! the bridge can see, so it counts that on its own clock and the bench divides
//! two of its tallies.
//!
//! MEASURED, NEVER JUDGED. A Strict ack is an fsync, `F_FULLFSYNC` on macOS,
//! and its cost is the disk's: an absolute floor here would fail on a slower
//! machine and pass a regression on a faster one. So there is no assertion on
//! any number — the checks that ARE here are about the measurement being of
//! what it says (the row that woke the reader is the message just posted; every
//! message was seen exactly once, with no stray row between; a row's offset is
//! its record's; each idle-bridge wait waited for the delivery's commit; the
//! burst's commit landed before the fence that bounds its window; each
//! ping-pong wake is its round's one message, and the two wait tallies are one
//! bridge process's, past the warm-up and past the last reply; the attach rows
//! are the restarted bridge's own).
//!
//! Run it through the perf lane, which builds it, sets [`spec::ENV`], and
//! writes the ledger entry under `docs/measured/fabric/`:
//!
//! ```text
//! targo --unverified verify perf fabric [--profile dev|opt] [--latency-samples N]
//!     [--burst N] [--pingpong-rounds N] [--attach-sessions 1,8,32] [--attach-rounds N]
//!     [--json-out PATH]
//! ```
//!
//! `--profile` picks the build: the dev code generation (the default) or the
//! optimized `opt-test` one, both with the test seams compiled in ([`profile`]).
//! The report names the arm from the `aterm` binary the run launched.
//!
//! The entry is `#[ignore]`d and does nothing unless [`spec::ENV`] is set, so
//! an ordinary run of this binary is its pure helpers' tests and nothing more.
#![cfg(unix)]

#[path = "../fabric_world/harness.rs"]
mod harness;

mod census;
mod clock;
mod profile;
mod run;
mod spec;
mod stats;

/// The measurement. See the module note.
#[test]
#[ignore = "a measurement over live stacks, minutes long: `verify perf fabric` runs it"]
fn measure() {
    let Some(spec) = spec::from_env() else {
        eprintln!(
            "fabric bench: {} is unset, so nothing is measured (`targo --unverified verify perf \
             fabric` sets it)",
            spec::ENV
        );
        return;
    };
    std::fs::create_dir_all(&spec.out)
        .unwrap_or_else(|e| panic!("create the report dir {}: {e}", spec.out.display()));
    let report = run::measure(&spec);
    let json = aterm_json::to_string(&report.json).expect("the report serializes");
    let json_path = spec.out.join("fabric.json");
    let md_path = spec.out.join("fabric.md");
    std::fs::write(&json_path, json)
        .unwrap_or_else(|e| panic!("write {}: {e}", json_path.display()));
    std::fs::write(&md_path, &report.markdown)
        .unwrap_or_else(|e| panic!("write {}: {e}", md_path.display()));
    println!("{}", report.markdown);
    eprintln!(
        "fabric bench: wrote {} and {}",
        json_path.display(),
        md_path.display()
    );
}
