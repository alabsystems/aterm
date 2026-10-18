// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS TAPES: a supervisor loop recorded so that it can be run again,
//! offline and exactly, and asked why it pressed what it pressed.
//!
//! A tape is every value the loop thread observed from outside itself, in the
//! order it observed them, interleaved with every output it produced; a
//! replay feeds the values back in order and checks each request and each
//! output where it occurs. Before the first value comes the loop's SETUP:
//! everything it was given before it ran — the target and the holder its
//! claim is taken under, its ledger and its timings
//! ([`super::run::SessionSetup`]), its options
//! ([`super::run::SuperviseOpts`]), the approval policy's process inputs
//! ([`super::run::ApprovalEnv`]) and the owner's policy itself — which a
//! replay rebuilds the loop from, so it never reads this process, this
//! machine's clock or files to learn what the recorded run was told.
//!
//! WHAT A TAPE HOLDS, AND WHAT IT DOES NOT YET. This build tapes what the
//! loop reads through its door (`supervise::fx::Fx`): the control lane, the
//! clocks, the files it seeds from, the host's flags, its outputs and decisions — and the looks at the machine that the approval
//! policy and the loop make, as DERIVED doors (`probe.<name>`, [`doors`]):
//! a hidden twin of a path, where a path's links lead, the worker (by a
//! digest, never its environment), git's configuration projected to the keys
//! the policy reads, the settings' and the shell's verdicts, the time zone,
//! the Codex lead and records. Each holds what was concluded, never what was
//! read to reach it (`policy::probe`). And the WINDOW'S HOST (`IdleHost`): its
//! every answer and every notification the loop gave it, at `host.<name>`
//! ([`host`]) — the host's own work inside a call (an upgrade's step, a
//! relaunch) frozen as the answer it gave. Not on a tape: the mail lane's
//! deliveries (a run that records refuses `--mail`).
//!
//! WHAT A REPLAY CLAIMS ([`replay`], `aterm drive replay FILE`): a STRICT
//! replay — on the build that recorded the tape — rebuilds the loop from the
//! header and re-runs it fed by the tape alone, every request and output
//! checked where it occurs; it is REPRODUCED, or the first divergence is named
//! with its place, door and class ([`report`]). Every whole run of this
//! crate's tests that its helpers record (`watch_lines`, `watch_quick`, the
//! hosted helpers…) is replayed so, twice (the equivalence suite,
//! `testing::assert_replays`): a value the loop reads past its door is a test
//! that fails — runs whose options carry a WINDOW HOST
//! (`SuperviseOpts::idle_host`) among them, the stall, survey, idle-point
//! and hosted tests, whose host is replayed by the replay's host from the
//! tape alone. Not yet: counterfactual replay (another policy, the acts
//! mode), shadow decisions and `why`.
//!
//! * [`header`]: that setup as the tape's header, a text of `key=value`
//!   lines whose policy block is rebuilt through the same
//!   [`super::SupervisorConfig::set`] the owner's file is read with.
//! * [`codec`]: the values themselves, as items — astream-effects'
//!   `EffectRecord`s beside aterm's notes, marks and end — and their bytes:
//!   varints, interned names, clocks as steps, and each reply stored against
//!   the last one to the same request ([`delta`]).
//! * [`frame`]: the file — a magic, the header frame, then records frames,
//!   each one `write(2)` with its own xxh64 — and the writer that closes them
//!   at each act, each wait, each mark and the end, and never fails the loop.
//! * [`recover`]: the file read back, as a pure function of its bytes: the
//!   whole frames, a torn tail dropped and said, damage inside refused, and a
//!   tape made by hand to cost more than its size refused by name.
//! * [`doors`]: the names of the doors a value crosses by, append-only.
//! * [`host`]: the window host's door — the taped host a recording loop asks
//!   and the replay's host, both generated with the trait
//!   (`run::host_door`), and the tape form of the host's arguments and
//!   answers.
//! * [`core`]: the RECORDER a run's door hands each value it observed, which
//!   tapes it in order through astream-effects' recording seam into the
//!   writer; [`ctl`]: the control lane's door, wrapping a loop's transport;
//!   [`store`]: the file a run's `--tape` names, made new and private.
//! * [`text`]: a tape as text, one record per line, every screen line by
//!   line, that reads back to the very same items; [`show`]: what a reader
//!   asks of a tape without replaying it — each decision with the screen it
//!   judged, the act and the journal line (`tape show --decisions`), and its
//!   size per hour (`tape stats`).
//! * [`replay`]: the tape run again — the core a replaying door asks in place
//!   of its live world (astream-effects' replay seam over the records), the
//!   control lane with no transport (`ctl::ReplayCtl`), and the driver;
//!   [`report`]: what a replay says, as values.
//!
//! WHAT A TAPE COSTS, MEASURED (2026-10-17, `aterm drive tape stats`): a
//! fifteen-minute `aterm drive watch --tape` of Claude Code 2.1.302 in the
//! real-Claude lab (`targo --unverified lab`: its local stub API, eight turns
//! ninety seconds apart), the loop watching behind the lab's own hosted
//! supervisor — idle twenty-second steps of `await agent …`, the claim's
//! refused renewal and a `status` — taped 677 effects in 48 frames, 48,088
//! bytes: 192.3 kB an hour, about 4.6 MB a day. The `status` reply is most of
//! it: it changes at `since_ms=` near its head and at `agent_since_ms=` far
//! into it, so the per-key delta's one prefix and one suffix ([`delta`])
//! keep most of it as the middle, about a kilobyte a step. The plan's §8
//! guessed idle hours "mostly repeat"; with this `status` they do not. The
//! tape's text form ([`text`]) read back byte for byte.
//!
//! Tapes are local files only: nothing here is put on the fabric, nothing is
//! installed into an agent, and no model or network is involved. The worker's
//! environment is never written; a stand-in for it is named by its kind.

pub mod codec;
pub(crate) mod core;
pub(crate) mod ctl;
pub(crate) mod delta;
pub mod doors;
pub mod frame;
pub mod header;
pub(crate) mod host;
pub mod recover;
pub mod replay;
pub mod report;
pub mod show;
pub mod store;
pub mod text;

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
#[path = "frame_tests.rs"]
pub(crate) mod frame_tests;
#[cfg(test)]
pub(crate) mod testing;
