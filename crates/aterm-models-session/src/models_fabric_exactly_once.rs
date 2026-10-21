// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EXACTLY ONCE ACROSS THE BRIDGE'S ROUND (`aterm-link`'s `bridge.rs`: the inbox
//! group drained into `deliver`, committed only once a record is accounted for;
//! aterm-gui-fabric's `cmd_deliver`, which takes one row per offset on the log it was
//! told, and `cmd_link_log`, which forgets the offsets a move took): one OFFSET of
//! this node's inbox lane, `O`, followed through every record that sits there, every
//! delivery, commit, kill and relaunch of the bridge, and every move of the log's
//! generation.
//!
//! The record `r` lands at `O` (`Publish`). The group hands it to the bridge, which
//! `deliver`s it — a new ROW at the endpoint unless the endpoint remembers `O` (`dd`,
//! its dedup) — and then owes the group's commit through `O` (`pend`); `Commit` moves
//! the group's resume point `c` past `O`. `Refill` is the admission's other road: the
//! session's lane read from its seen point, the record offered past any cursor. A
//! `Kill` loses what the bridge owed; `Relaunch` starts the next one.
//!
//! A MOVE of the log's generation is a power loss under a weak tier (or a repair's
//! cut) that the broker announces with lineage. `MoveTakes` is one whose fork is at
//! or below `O`: the record there is lost, the group's commit through it too (its
//! commit record sits above it, and the log keeps a prefix), and a LATER record `r2`
//! may land at the same offset (`Reuse`). `MoveKeeps` is one whose fork is above
//! `O`: the record stays; `MoveKeepsUncommits` is the same move taking only the
//! commit record, so the group offers `O` again. The bridge learns of a move at its
//! next attach, and TELLS the endpoint before it delivers or commits anything
//! (`Tell`, `link log <id> continues=<fork> was=<old>`): after a move that took `O`
//! the endpoint forgets it (lane 3) and the bridge forgets the commit it owed
//! through it; after one that kept `O`, both keep theirs. Before the tell, the bridge
//! may still deliver what its old connection had already handed it — the record at
//! `O` the move kept (`Deliver`), or the one it took (`DeliverTaken`: a row of a
//! record the log no longer holds, and a commit owed through `O` that the tell must
//! forget) — but nothing of the moved log: that comes on the attach's own
//! subscriptions, which the tell precedes.
//!
//! COMPOSED WITH THE READER-RESUME LAW, not a second clamp model. Every offset this
//! node holds is clamped at a move by `forget_log_offsets`, the clamp
//! `fabric_reader_resume_model` (`FabricReaderResume`) proves never skips a record
//! posted after a fork and whose Tier-1 (`aterm-link`'s `state.rs`,
//! `the_reader_resume_refines_its_model`) binds to the code. Here that clamp is TAKEN
//! AS GIVEN where it meets the round: told of a move whose fork is at or below `O`,
//! the bridge forgets the commit it owed through `O` (`pend = 0`), as the law's resume
//! point — at or below the lowest fork — says it must. `Buggy = 5` is that clamp's
//! absence, and it breaks exactly-once here as the law's own mutants break the resume.
//!
//! Invariants. `NoDuplicate`: no record is a row twice (`nr`, `nr2` count the rows of
//! `r` and `r2`). `NoSkip`: the group's resume point never passes the record that
//! sits at `O` unless that record is a row — what an at-least-once cursor and an
//! idempotent sink owe together.
//!
//! `Buggy`, one defect each: 1 an endpoint with no dedup (every delivery a row) · 2
//! the commit made before the delivery (`CommitEarly`) · 3 an endpoint that never
//! forgets an offset a move took (`r2` is answered as `r`'s row) · 4 an endpoint that
//! forgets every offset at any move (lane 2: `r` redelivered is a second row) · 5 the
//! owed commit kept across a move that took `O` (the reader-resume clamp's absence).
//!
//! Tier-0 is `tests/derived_fabric_exactly_once.rs`. Tier-1 is `aterm-link`'s
//! deterministic simulation (`sim`, `the_round_refines_its_exactly_once_model`): every
//! inbox offset of every committed seed, projected onto this model step by step — the
//! real bridge's deliveries and commits, the broker model's moves, the endpoint's
//! dedup — must be the model's, and a world with the endpoint's dedup switched off
//! must not be.

use aterm_spec::derive::Model;

/// Exactly once across the bridge's round, at one inbox offset (module doc). `O` is
/// the offset, `MaxG` bounds the moves, `MaxRows` the rows one offset can make.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_exactly_once_model() -> Model {
    aterm_spec::ty_model! {
        FabricExactlyOnce {
            const Buggy = 0;
            const O = 1;
            const MaxG = 2;
            const MaxRows = 3;
            var occ = 0;
            var lost = 0;
            var g = 0;
            var c = 0;
            var pend = 0;
            var alive = 1;
            var dd = 0;
            var nr = 0;
            var nr2 = 0;
            var told = 1;
            var mv = 0;

            action Publish when (occ == 0 && lost == 0 && nr == 0) {
                occ = 1;
            }
            action Reuse when (occ == 0 && lost == 1 && nr2 == 0) {
                occ = 2;
            }
            action Deliver when (alive == 1 && (told == 1 || occ == 1) && occ > 0 && O + 1 > c && pend == 0 && MaxRows > nr + nr2) {
                nr = if occ == 1 && (dd == 0 || Buggy == 1) { nr + 1 } else { nr };
                nr2 = if occ == 2 && (dd == 0 || Buggy == 1) { nr2 + 1 } else { nr2 };
                dd = 1;
                pend = 1;
            }
            action DeliverTaken when (alive == 1 && told == 0 && mv == 1 && lost == 1 && pend == 0 && MaxRows > nr + nr2) {
                nr = if dd == 0 || Buggy == 1 { nr + 1 } else { nr };
                dd = 1;
                pend = 1;
            }
            action Refill when (alive == 1 && told == 1 && occ > 0 && MaxRows > nr + nr2) {
                nr = if occ == 1 && (dd == 0 || Buggy == 1) { nr + 1 } else { nr };
                nr2 = if occ == 2 && (dd == 0 || Buggy == 1) { nr2 + 1 } else { nr2 };
                dd = 1;
            }
            action Commit when (alive == 1 && told == 1 && pend == 1) {
                c = O + 1;
                pend = 0;
            }
            action CommitEarly when (Buggy == 2 && alive == 1 && told == 1 && occ > 0 && O + 1 > c && pend == 0) {
                c = O + 1;
            }
            action Kill when (alive == 1) {
                alive = 0;
                pend = 0;
            }
            action Relaunch when (alive == 0) {
                alive = 1;
            }
            action MoveTakes when (occ == 1 && MaxG > g) {
                occ = 0;
                lost = 1;
                g = g + 1;
                c = if c <= O { c } else { O };
                told = 0;
                mv = 1;
            }
            action MoveKeeps when (occ + lost > 0 && MaxG > g) {
                g = g + 1;
                told = 0;
                mv = if mv == 1 { 1 } else { 2 };
            }
            action MoveKeepsUncommits when (occ > 0 && MaxG > g && c > O) {
                g = g + 1;
                c = O;
                told = 0;
                mv = if mv == 1 { 1 } else { 2 };
            }
            action Tell when (alive == 1 && told == 0) {
                dd = if mv == 1 {
                    if Buggy == 3 { dd } else { 0 }
                } else {
                    if Buggy == 4 { 0 } else { dd }
                };
                pend = if mv == 1 {
                    if Buggy == 5 { pend } else { 0 }
                } else {
                    pend
                };
                told = 1;
                mv = 0;
            }

            invariant NoDuplicate: nr <= 1 && nr2 <= 1;
            invariant NoSkip: O + 1 > c || occ == 0 || (occ == 1 && nr > 0) || (occ == 2 && nr2 > 0);
            bound MovesBounded: g <= MaxG;
            bound CursorBounded: c <= O + 1;
            bound RowsBounded: nr <= MaxRows;
            bound RowsTwoBounded: nr2 <= MaxRows;
        }
    }
}
