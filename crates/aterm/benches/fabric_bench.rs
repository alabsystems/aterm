// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC MEASUREMENT: what aterm's use of the astream bus costs, as
//! numbers, so every later speed claim about it is one.
//!
//! It stands up the REAL stack, the way the fabric worlds do — it mounts their
//! harness (`fabric_world/harness.rs`) and boots `harness::World`s: astream's
//! broker opened as aterm opens it (`Broker::open_opts` with aterm's own
//! options for the run's tier, `harness::broker_opts`: ordered at aterm's
//! horizon — what aterm opens its guarded broker at by default — unless the
//! arm names strict or relaxed, with aterm's escalation table on a weak tier),
//! the headless `aterm` this package builds, and the `aterm link serve` bridge
//! that `aterm` launches and supervises. Nothing is stubbed: a message here
//! travels every hop a person's does. It reports, each as `n` with mean and
//! nearest-rank p50/p95/p99 ([`stats`]):
//!
//! 1. **post -> deliver**: one session posts to another on the same node; from
//!    the `post` reply to the row visible in the recipient's inbox (its
//!    `await inbox` answering), split at the moment the record was ACKED on
//!    the broker and, apart, the moment it was DURABLE ([`clock`]: on a strict
//!    broker the two are one).
//! 2. **burst throughput**: N posts back to back to one recipient; when all N
//!    were accepted, acked, visible, durable, and covered by the recipient
//!    bridge's inbox commit, and each message's own post -> visible.
//! 3. **ping-pong**: A posts to B and B replies the moment it sees the message,
//!    back to back (default 200 rounds): the round trip, its legs acked and
//!    durable apart, each reply's wait for the inbox commit of the delivery it
//!    answers, and the share of the window the bridge's one loop spent blocked
//!    on broker replies, counted inside the bridge by its wait-tally seam
//!    (`aterm_link::wait_tally`).
//! 4. **attach time**: a bridge starting with N sessions (default 1, 8, 32),
//!    from its pid file to every session's presence row acked, and durable,
//!    again.
//! 5. **records per delivered message**: every record the broker wrote over
//!    each measured window, hidden commits included, by kind ([`census`]) —
//!    the count the bridge's batching reduces (in the burst, how far depends
//!    on scheduling: the README says which counts compare) — and the broker's
//!    barrier, full and skipped flushes per delivered message (per round trip
//!    too, in the ping-pong).
//!
//! How the times are taken, and why they need no clock inside the stack, is
//! [`clock`]; what each phase does is [`run`]. The one count taken inside it is
//! the ping-pong's: how long the bridge's loop waited on the broker, which only
//! the bridge can see, so it counts that on its own clock and the bench divides
//! two of its tallies.
//!
//! MEASURED, NEVER JUDGED. A Strict ack is an fsync, `F_FULLFSYNC` on macOS
//! (an ordered one an `F_BARRIERFSYNC`), and its cost is the disk's: an
//! absolute floor here would fail on a slower
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
//!     [--durability strict,ordered,relaxed] [--horizon-ms 1000,100] [--passes N]
//!     [--json-out PATH]
//! ```
//!
//! `--durability` names the arms, each one run of this bench and one ledger
//! entry, an ordered arm once per `--horizon-ms`; the bench itself runs one
//! arm, `durability=<tier> [horizon_ms=<n>]` in [`spec::ENV`].
//!
//! `--profile` picks the build: the dev code generation (the default) or the
//! optimized `opt-test` one, both with the test seams compiled in ([`profile`]).
//! The report names the arm from the `aterm` binary the run launched.
//!
//! This explicit `harness = false` bench requires [`spec::ENV`]. Ordinary
//! tests run the shared pure helpers without booting any measurement stacks.

#[cfg(unix)]
#[path = "../tests/fabric_world/harness.rs"]
mod harness;

#[cfg(unix)]
#[path = "../tests/fabric_bench/census.rs"]
mod census;
#[cfg(unix)]
#[path = "../tests/fabric_bench/clock.rs"]
mod clock;
#[cfg(unix)]
#[path = "../tests/fabric_bench/profile.rs"]
#[expect(
    dead_code,
    reason = "the verify perf driver also uses this shared profile table's build and CLI methods"
)]
mod profile;
#[cfg(unix)]
#[path = "../tests/fabric_bench/run.rs"]
mod run;
#[cfg(unix)]
#[path = "../tests/fabric_bench/spec.rs"]
mod spec;
#[cfg(unix)]
#[path = "../tests/fabric_bench/stats.rs"]
mod stats;

/// The measurement. See the module note.
#[cfg(unix)]
fn main() -> Result<(), String> {
    let spec = spec::from_env()?;
    std::fs::create_dir_all(&spec.out)
        .map_err(|e| format!("create the report dir {}: {e}", spec.out.display()))?;
    let report = run::measure(&spec);
    let json = aterm_json::to_string(&report.json).map_err(|e| format!("serialize report: {e}"))?;
    let json_path = spec.out.join("fabric.json");
    let md_path = spec.out.join("fabric.md");
    std::fs::write(&json_path, json).map_err(|e| format!("write {}: {e}", json_path.display()))?;
    std::fs::write(&md_path, &report.markdown)
        .map_err(|e| format!("write {}: {e}", md_path.display()))?;
    println!("{}", report.markdown);
    eprintln!(
        "fabric bench: wrote {} and {}",
        json_path.display(),
        md_path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() -> Result<(), String> {
    Err("fabric measurement requires Unix".to_string())
}
