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
//! relaunch) frozen as the answer it gave. And a tasked session's result at
//! its done edge (`doors::TASK_RESULT`): what the transcript settled to and
//! the snapshot written, never read or written again by a replay.
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
//! tape alone.
//!
//! WHAT A COUNTERFACTUAL CLAIMS ([`acts`], `aterm drive replay FILE --set
//! k=v…|--policy FILE`): the recorded loop run again under a CANDIDATE
//! policy, the acts held to the tape and the reads served from their
//! segment, up to the first act the candidate takes otherwise — both sides
//! named — or a read the tape never answered there (STALE); never past it,
//! never re-driving the agent. Every tape of the equivalence suite also
//! replays so under its own policy, record for record. The SHADOW
//! ([`shadow`], `--shadow`): every recorded decision decided again under the
//! candidate, point by point, TAINT after the first that differs.
//!
//! WHAT `--why` CLAIMS ([`explain`], `aterm drive replay FILE --why --at …`):
//! one decision on a tape explained — the screen, the box, the facts and
//! their doors, the act, the journal line and the ledger row — with the
//! policy's rule trace computed only here, never by a running loop: an
//! approval's or a question's by deciding again on the facts its mark
//! carries (any build; what it lacked named), a turn end's in a strict
//! replay to the point (the recording build only).
//!
//! * [`header`]: that setup as the tape's header, a text of `key=value`
//!   lines whose policy block is rebuilt through the same
//!   [`super::SupervisorConfig::set`] the owner's file is read with.
//! * [`codec`]: the values themselves, as items — astream-effects'
//!   `EffectRecord`s beside aterm's notes, marks and end — and their bytes:
//!   varints, interned names, clocks as steps, and each reply stored against
//!   the last one to the same request ([`delta`]).
//! * [`frame`]: the file — a magic, the header frame, then records frames,
//!   each one `write(2)` with its own xxh64, which names the frame's place
//!   (frame format 2; format 1 tapes are still read) — and the writer that
//!   closes them at each act, each wait, each mark and the end, and never
//!   fails the loop.
//! * [`recover`]: the file read back, as a pure function of its bytes: the
//!   whole frames, a torn tail dropped and said, damage inside refused — a
//!   frame changed, or (frame format 2) whole frames repeated, moved, or
//!   dropped from inside the tape; one that lost its last frames reads as a
//!   run that stopped there, without its end — and a tape made by hand to
//!   cost more than its size refused by name.
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
//! * [`explain`]: `--why` — a decision explained, its trace computed from the
//!   tape (an approval decided again on its facts through a keyed probe, a
//!   turn end in a replay to the point).
//! * [`replay`]: the tape run again — the core a replaying door asks in place
//!   of its live world (astream-effects' replay seam over the records, or
//!   the acts cursor for a counterfactual), the control lane with no
//!   transport (`ctl::ReplayCtl`), and the driver; [`report`]: what a replay
//!   says, as values.
//! * [`acts`]: a counterfactual's cursor — the tape cut into segments by its
//!   acts, the candidate policy it runs under; [`shadow`]: every decision
//!   decided again under a candidate.
//! * [`corpus`]: hosted-loop tapes the lab recorded, kept as regression
//!   cases — each replayed in acts mode under its own policy (`aterm drive
//!   replay --check FILE|DIR…`, `tests/tape_corpus.rs`), and made portable
//!   before it is kept: the recording machine's names scrubbed to fixed
//!   tokens, linted, and refused unless the scrubbed tape still replays as
//!   the original did (`targo --unverified lab tape promote`).
//!
//! WHAT A TAPE COSTS, MEASURED (2026-10-17, `aterm drive tape stats`): a
//! fifteen-minute `aterm drive watch --tape` (since removed) of Claude Code 2.1.302 in the
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
//! WHAT A SHADOW AND A `--why` COST, MEASURED (2026-10-20,
//! `benches/tape_shadow_cost.rs`, opt-test, medians, an 18-core machine at a
//! load of 13-24): no recorded tape holds thousands of decisions, so the
//! bench grows them from two corpus runs of the real hosted loop — the
//! approval cycle repeated in place, clocks moved on, nothing else edited,
//! every box shadowed as recorded before anything is timed. At 1k / 10k / 30k
//! / 100k effects (14 / 194 / 594 / 1,994 boxes, one box per 50 effects, the
//! dense end):
//! * `c1-approval`'s Bash box, decided on its facts alone: `--shadow` 1.0 /
//!   11 / 33 / 111 ms, a flat 0.055-0.068 ms a box beside a read of the tape
//!   (`show::open`) of 0.11 / 0.79 / 2.2 / 7.0 ms;
//! * `c4-read`'s Read box, decided on two looks at the disk: 1.1 / 37 / 261
//!   / 2,593 ms when each look built a keyed view of the whole tape (0.60 ms
//!   at 100k) — 0.077 rising to 1.30 ms a box, quadratic — and 0.9 / 11 / 34
//!   / 119 ms, a flat 0.057-0.062 ms a box, since every probe on a tape asks
//!   one shared index (astream-effects' `KeyedIndex`, [`explain`]'s `Book`);
//! * `--why --at` one box: 0.3 / 2.5 / 7.7 / 28 ms for either seed, before
//!   and after, about four reads of the tape (it decodes the tape and walks
//!   it once to explain one point).
//!
//! Those are `--shadow` on a tape ANOTHER build recorded, each turn end
//! listed and not decided again (the bench's build is never a tape's
//! recording build), and the boxes' share of it on any build. On the build
//! that recorded the tape `--shadow` also replays the whole loop STRICTLY to
//! decide the turn ends again ([`shadow`]), which no grown tape measures: a
//! copy is the seed's bytes with its clocks moved, not a run the loop makes
//! again. On the seeds themselves (391 / 399 effects; the merged T12 tree at
//! a load of ~45): 0.2 ms as another build's, 1.4 / 1.2 ms stamped with the
//! shadowing build, the strict replay alone 1.1 / 1.0 ms — and even that
//! replay stopped at the seed's box (#265 / #276: this build's mark carries
//! facts the recording build did not write), so 2 of 3 turn ends were
//! decided again. What a same-build `--shadow` costs on a long run is not
//! measured.
//!
//! Tapes are local files only: nothing here is put on the fabric, nothing is
//! installed into an agent, and no model or network is involved. The worker's
//! environment is never written; a stand-in for it is named by its kind.

pub mod acts;
pub mod codec;
pub(crate) mod core;
pub mod corpus;
pub(crate) mod ctl;
pub(crate) mod delta;
pub mod doors;
pub mod explain;
pub mod frame;
pub mod header;
pub(crate) mod host;
pub mod recover;
pub mod replay;
pub mod report;
pub mod shadow;
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
