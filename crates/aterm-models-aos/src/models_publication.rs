// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Same-schema boundary publication: how a hosted task's live (code, state) pair is replaced at
//! a step boundary without mixing versions, losing an input or letting a stale candidate win.
//!
//! The protocol is the Aos plan's §4 (Trust `docs/plans/2026-10-21-aos-hosted-execution.md`) as
//! aterm builds it (`docs/DESIGN-aos-hosted-execution-2026-10-21.md` §3.9,
//! `aterm_aos::publish::Boundary`). The task pauses between two `step` calls and the paused live
//! head is captured; a candidate is prepared against that head (its base); inputs that arrive
//! while the task is paused wait in a bounded queue; and the update ends in one of three ways:
//!
//! * **commit**, only while the live head still equals the candidate's base: code, replayed
//!   state and a new revision are published together, then each queued input is delivered once,
//!   in order, to the new pair;
//! * **reject**: the old pair resumes and each queued input is delivered to it once, in order;
//! * **overflow**: an input that finds the queue full rejects the update before anything is
//!   dropped; the queue and then that input go to the old pair.
//!
//! A prepared candidate is a value its holder keeps or drops (discard). The head moves whenever an
//! input is delivered or a revision is published, so a candidate kept across that move is stale
//! and can never be published: publication compares the whole head, never the candidate's own
//! new code or state, and never its older replay ancestor.

use aterm_spec::derive::Model;

/// One hosted task's publication boundary, with two candidate slots so that two candidates
/// prepared against the same head can race (the stale-winner case, Aos plan §4).
///
/// State. `paused`: the task is held at a boundary. `rev` and `cur`: the live head's revision and
/// input cursor (inputs folded into the live state), relative to attach. `code_v` and `state_v`:
/// the publication that installed the live code and the one that installed the live state (0 is
/// the attached pair; each commit is the next). `arrived`: inputs offered to the task (a skipped
/// tick or query is no input here); `queued`: those waiting for the boundary; `delivered`: those
/// handed to some pair's step. `pa`/`pb`: slot A or B holds a prepared candidate, whose base head
/// is (`ra`, `ia`) or (`rb`, `ib`) (zero while the slot is empty). `stale`: a candidate whose base
/// was no longer the live head was published.
///
/// Actions. `Input` (running: the input runs at once), `Pause`, `PrepareA`/`PrepareB` (replay a
/// candidate against the paused head), `Hold` (paused: the input queues), `Overflow` (paused, the
/// queue full: the update is rejected and the queue, then the input, reach the old pair),
/// `CommitA`/`CommitB`, `Reject`, `DiscardA`/`DiscardB` (the holder drops a candidate; a refused
/// commit consumes its candidate the same way).
///
/// Laws, each broken by its own `Buggy` dial and by no other:
///
/// * `NeverMixed` — the live code and the live state come from the same publication: no new code
///   over old state, no old code over new state. `Buggy = 2`: commit publishes the code alone (a
///   dispatch-pointer swap over the old state).
/// * `EachInputOnce` — every input that arrived is delivered or still queued: none lost, none
///   delivered twice. `Buggy = 3`: reject resumes the old pair but drops the queue.
/// * `StaleNeverCommitted` — no candidate is published once the live head has moved past its
///   base. `Buggy = 1`: commit compares the input cursor and forgets the revision, so the second
///   of two candidates prepared against one head overwrites the first one's publication.
/// * `DeliveredToTheLivePair` — the live state has folded every delivered input exactly once
///   (its input cursor counts them): each queued input reaches the pair that stays live, never one
///   that a publication then replaces. `Buggy = 4`: commit delivers the queue to the old pair and
///   then installs the candidate's replayed state, which never saw those inputs.
///
/// Bounds (the explored box, which no dial breaks): `ArrivalsBounded` (`MaxIn` inputs),
/// `QueueBounded` (the queue's capacity `Cap`, enforced by `Hold`'s guard) and `RevisionsBounded`
/// (`MaxRev` publications; the task pauses only while another fits). The scenario `Cap = 0` is a
/// boundary built with no queue, where every input that arrives while paused overflows.
///
/// What this cannot state: the order of delivery (the model is scalar; the Tier-1 test checks
/// order on the real queue), and anything about the replay that produced the candidate's state
/// (that is the witness's job, `aterm_aos::evidence`).
///
/// Tier-1: `aterm-aos`'s `tests/conformance_publication.rs` drives the real `Boundary` with a toy
/// schema in lock step with this model, projects each state onto these variables and checks every
/// real step against the model, along seeded walks and along the shortest path to every reachable
/// state with every move the model allows from it. It has a negative control per dial: the real
/// boundary refuses the commit the stale mutant publishes (1), never shows the mixed mutant's
/// successor after any store a commit can stop at (2), and takes a step the lost-input (3) and
/// wrong-pair (4) mutants do not admit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn aos_boundary_publication_model() -> Model {
    aterm_spec::ty_model! {
        AosBoundaryPublication {
            const Buggy = 0;
            const MaxIn = 3;
            const Cap = 2;
            const MaxRev = 2;
            scenario Cap = 0;
            var paused = 0;
            var rev = 0;
            var cur = 0;
            var code_v = 0;
            var state_v = 0;
            var arrived = 0;
            var queued = 0;
            var delivered = 0;
            var pa = 0;
            var ra = 0;
            var ia = 0;
            var pb = 0;
            var rb = 0;
            var ib = 0;
            var stale = 0;

            // Running: the input reaches the live pair at once and the head moves.
            action Input when (paused == 0 && arrived + 1 <= MaxIn) {
                arrived = arrived + 1;
                delivered = delivered + 1;
                cur = cur + 1;
            }
            action Pause when (paused == 0 && rev + 1 <= MaxRev) {
                paused = 1;
            }
            // A candidate's base is the paused head, never its own new code or state.
            action PrepareA when (paused == 1 && pa == 0) {
                pa = 1;
                ra = rev;
                ia = cur;
            }
            action PrepareB when (paused == 1 && pb == 0) {
                pb = 1;
                rb = rev;
                ib = cur;
            }
            action Hold when (paused == 1 && arrived + 1 <= MaxIn && queued + 1 <= Cap) {
                arrived = arrived + 1;
                queued = queued + 1;
            }
            // A full queue rejects the update before any input is dropped.
            action Overflow when (paused == 1 && arrived + 1 <= MaxIn && queued == Cap) {
                paused = 0;
                arrived = arrived + 1;
                delivered = delivered + queued + 1;
                cur = cur + queued + 1;
                queued = 0;
            }
            action CommitA when (paused == 1 && pa == 1 && ia == cur && (ra == rev || Buggy == 1)) {
                paused = 0;
                pa = 0;
                ra = 0;
                ia = 0;
                rev = rev + 1;
                code_v = code_v + 1;
                state_v = if Buggy == 2 { state_v } else { state_v + 1 };
                stale = if ra == rev && ia == cur { stale } else { 1 };
                cur = if Buggy == 4 { ia } else { ia + queued };
                delivered = delivered + queued;
                queued = 0;
            }
            action CommitB when (paused == 1 && pb == 1 && ib == cur && (rb == rev || Buggy == 1)) {
                paused = 0;
                pb = 0;
                rb = 0;
                ib = 0;
                rev = rev + 1;
                code_v = code_v + 1;
                state_v = if Buggy == 2 { state_v } else { state_v + 1 };
                stale = if rb == rev && ib == cur { stale } else { 1 };
                cur = if Buggy == 4 { ib } else { ib + queued };
                delivered = delivered + queued;
                queued = 0;
            }
            action Reject when (paused == 1) {
                paused = 0;
                delivered = if Buggy == 3 { delivered } else { delivered + queued };
                cur = if Buggy == 3 { cur } else { cur + queued };
                queued = 0;
            }
            action DiscardA when (pa == 1) {
                pa = 0;
                ra = 0;
                ia = 0;
            }
            action DiscardB when (pb == 1) {
                pb = 0;
                rb = 0;
                ib = 0;
            }

            invariant NeverMixed: code_v == state_v;
            invariant EachInputOnce: delivered + queued == arrived;
            invariant StaleNeverCommitted: stale == 0;
            invariant DeliveredToTheLivePair: cur == delivered;
            bound ArrivalsBounded: arrived <= MaxIn;
            bound QueueBounded: queued <= Cap;
            bound RevisionsBounded: rev <= MaxRev;
        }
    }
}
