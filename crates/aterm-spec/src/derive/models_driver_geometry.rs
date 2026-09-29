// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DRIVER'S GEOMETRY: a driver's own lease never moves the screen its
//! `if-gen=` fence was judged on.
//!
//! Measured on the owner's Mac on 2026-09-27: the live upgrade's notice into
//! an idle Claude Code was refused `skipped reason=changed` on 121 visits of
//! 121. The turn took the drive lease, the lease woke the presence band, the
//! band's row was born on the quiet window — one PTY re-grid, a `SIGWINCH`, a
//! full repaint by Claude Code — and the fence, checked after the turn's
//! yield, read the repaint as a change every time. A `lease acquire` alone
//! re-gridded the tab the same way.

use super::Model;

/// A window's presence band row against a driver's fenced typing.
///
/// `hand` is the driven session's lease: `0` none, `1` a driver may type (a
/// `turn` before its settle phase, or a live drive lease —
/// `Lease::driver_may_type`), `2` a `turn` settling. `other` is a fact other
/// than the hand that wants the row (mail, a story). `rows` is the row the
/// window has committed; `gen` is the screen generation, which a re-grid
/// moves (the program repaints on the `SIGWINCH`) and so does the program's
/// own output. `Read` is the driver's judged read (`typing_fence`: `judged`,
/// `seen`); `Fence` is the turn's `if-gen=` check, under the terminal lock,
/// right before its paste. `Regrid` is the presence refresh committing the
/// row the facts want — a wake, so it can land anywhere.
///
/// `NoRegridUnderTheHand` — no re-grid lands while a driver may type into the
/// session. `OwnHandNeverRefusesTheFence` — a fence is never refused by a
/// re-grid the driver's own hand caused between its read and its paste. The
/// fence still refuses the program's own output (`Output` between `Read` and
/// `Fence` sets `refused`, never `refused_own`), and the row the hand wants is
/// committed once the input is over (`EndInput` lets `Regrid` fire).
///
/// EVERY DEFECT IS ITS OWN `Buggy = 1` ACTION, dead at the committed config,
/// so each is a negative control caught alone (`--strict-vacuity`) and the
/// progress claim below can name its own. `RegridUnderTheHand` is the
/// incident: the refresh commits the row whatever the hand —
/// `Read, Take, RegridUnderTheHand, Fence` is the owner's refused notice, and
/// a `Take, RegridUnderTheHand` alone is the `lease acquire` that resized the
/// tab.
///
/// THE SPLIT REFRESH (2026-09-28). `Regrid` is a refresh that reads the lease
/// and commits the row in one step; the real one reads the facts first
/// (`Sense`: `sensed`, the lease it saw `snap`, the row those words want
/// `snap_want`) and commits later (`Commit`), and the control thread's
/// `lease release` — or a `turn`'s end of input and its release, back to
/// back — can land between the two. Every change of the lease posts a wake
/// (`woke`), and a wake is what a `Sense` answers.
///
/// `Commit` is held by the LIVE hand (the host's `Desk::driver_may_type`) AND
/// by a lease that MOVED since the sense (`snap` against `hand`: the drive
/// module's `lease_moved`, the slot's `LeaseMark` against the host's live
/// one): words formed under one lease are never committed under another, and
/// the wake that moved it re-derives them. `NoBirthForAGoneHand` is the
/// consequence, stated on the LIVE facts, not on the guard: no row is born
/// while nothing live wants it (`hand == 0 && other == 0`) — the measured
/// flap, `lease acquire` then `lease release` under 0.1 ms apart, a row born
/// for a hand already gone and folded on the release's wake, 24 → 23 → 24 in
/// 0.35 ms, in 3 runs of 10.
///
/// `CommitLiveOnly` is the race as bb58ea333 shipped it (the live hold
/// only): `Take, Sense, Drop, CommitLiveOnly`. `CommitTypingOnly` is the
/// first repair, a hold on the snapshot's TYPING only: a turn's `EndInput`
/// then `Release` — `Take, EndInput, Sense, Release, CommitTypingOnly` —
/// still births the ghost row. Each breaks `NoBirthForAGoneHand` and nothing
/// else.
///
/// `EveryMovedLeaseOwesAWake` — words formed under a lease that has since
/// moved (`snap != hand`) always have a wake coming (`woke`): the hold waits
/// on nothing that is not already posted. `DropNoWake`, a release that posts
/// no wake, breaks it; [`driver_geometry_liveness`] is the progress half it
/// is the safety half of, and `DropNoWake` is that property's mutant too.
///
/// A PROJECTION A HOLD TURNS AWAY IS GONE (review of the integration,
/// 2026-09-29). The real commit is attempted by the projection that formed
/// the want, and a held one does not wait: `Turned` drops it (`sensed = 0`).
/// Nothing re-projects the window for the row but a wake whose facts MOVED —
/// the drive module's `refresh_session` projects nothing on a sense that
/// changed nothing. A second session the window shows (a background tab, a
/// split pane: `peer`) holds the row with its live lease too
/// (`Desk::driver_may_type` reads every session the window shows), and its
/// wakes (`pw`) re-project the window only when its slot's copy (`ps`) moves:
/// `PeerTake, PeerDrop, PeerSense` — a lease taken and let go before either
/// wake ran — projects nothing. `Catch` is the timer's answer, the drive
/// module's `rows_deadline`: a row count the last words want that no hold
/// stands against is owed a commit now. Without it the row a peer's lease
/// held never catches up (the band's words clock caught it a minute later on
/// a visible window, never on a headless one); [`driver_geometry_liveness`]
/// needs its fairness, which the audit proves load-bearing.
///
/// Tier-1 (`aterm-gui/src/driver_geometry_conformance.rs`) drives the real
/// `turn` and `lease` verbs on a real pty against the real App presence
/// projection, playing the event loop, the window's re-grid and an
/// alt-screen, focus-reporting program that repaints on every `SIGWINCH`, and
/// replays the incident's lease (no input phase) as its negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn driver_geometry_model() -> Model {
    crate::ty_model! {
        DriverGeometry {
            const Buggy = 0;
            const GenCap = 3;
            var hand = 0;
            var other = 0;
            var rows = 0;
            var gen = 0;
            var judged = 0;
            var seen = 0;
            var own = 0;
            var refused = 0;
            var refused_own = 0;
            var grids_under_hand = 0;
            var sensed = 0;
            var snap = 0;
            var snap_want = 0;
            var ghost = 0;
            var woke = 0;
            var peer = 0;
            var pw = 0;
            var ps = 0;

            action Read when (judged == 0 && hand <= 1) {
                judged = 1;
                seen = gen;
                own = 0;
            }
            action Take when (hand == 0) { hand = 1; woke = 1; }
            action Note when (other == 0) { other = 1; woke = 1; }
            action Output when (gen <= GenCap - 1) { gen = gen + 1; }
            action Regrid when (gen <= GenCap - 1
                && (hand == 0 || hand == 2)
                && peer == 0
                && ((rows == 0 && (hand > 0 || other == 1))
                    || (rows == 1 && hand == 0 && other == 0))) {
                rows = if hand > 0 || other == 1 { 1 } else { 0 };
                gen = gen + 1;
                own = if judged == 1 && hand == 1 { 1 } else { own };
                grids_under_hand = if hand == 1 { 1 } else { grids_under_hand };
            }
            action RegridUnderTheHand when (Buggy == 1 && gen <= GenCap - 1
                && hand == 1 && rows == 0) {
                rows = 1;
                gen = gen + 1;
                own = if judged == 1 { 1 } else { own };
                grids_under_hand = 1;
            }
            action Sense when (woke == 1) {
                woke = 0;
                sensed = 1;
                snap = hand;
                snap_want = if hand > 0 || other == 1 { 1 } else { 0 };
            }
            action Commit when (sensed == 1 && gen <= GenCap - 1
                && (hand == 0 || hand == 2)
                && peer == 0
                && snap == hand
                && (snap_want > rows || rows > snap_want)) {
                ghost = if rows == 0 && snap_want == 1 && hand == 0 && other == 0 {
                    1
                } else {
                    ghost
                };
                rows = snap_want;
                gen = gen + 1;
                own = if judged == 1 && hand == 1 { 1 } else { own };
                grids_under_hand = if hand == 1 { 1 } else { grids_under_hand };
                sensed = 0;
            }
            action CommitLiveOnly when (Buggy == 1 && sensed == 1 && gen <= GenCap - 1
                && (hand == 0 || hand == 2)
                && peer == 0
                && (snap_want > rows || rows > snap_want)) {
                ghost = if rows == 0 && snap_want == 1 && hand == 0 && other == 0 {
                    1
                } else {
                    ghost
                };
                rows = snap_want;
                gen = gen + 1;
                own = if judged == 1 && hand == 1 { 1 } else { own };
                grids_under_hand = if hand == 1 { 1 } else { grids_under_hand };
                sensed = 0;
            }
            action CommitTypingOnly when (Buggy == 1 && sensed == 1 && gen <= GenCap - 1
                && (hand == 0 || hand == 2)
                && peer == 0
                && (snap <= 0 || snap > 1)
                && (snap_want > rows || rows > snap_want)) {
                ghost = if rows == 0 && snap_want == 1 && hand == 0 && other == 0 {
                    1
                } else {
                    ghost
                };
                rows = snap_want;
                gen = gen + 1;
                own = if judged == 1 && hand == 1 { 1 } else { own };
                grids_under_hand = if hand == 1 { 1 } else { grids_under_hand };
                sensed = 0;
            }
            action Fence when (hand == 1 && judged == 1) {
                refused = if gen == seen { 0 } else { 1 };
                refused_own = if gen == seen { refused_own } else {
                    if own == 1 { 1 } else { refused_own }
                };
                judged = 0;
            }
            action EndInput when (hand == 1 && judged == 0) { hand = 2; woke = 1; }
            action Drop when (hand == 1 && judged == 0) { hand = 0; woke = 1; }
            action Release when (hand == 2) { hand = 0; woke = 1; }
            action DropNoWake when (Buggy == 1 && hand == 1 && judged == 0) { hand = 0; }
            // A projection a hold turns away: dropped, not kept for the lift.
            action Turned when (sensed == 1
                && (hand == 1 || peer == 1 || (snap <= hand - 1 || hand + 1 <= snap))) {
                sensed = 0;
            }
            // The other session the window shows: its lease, its wake, and
            // the projection of the window its sense makes only when its
            // slot's copy of the lease moved.
            action PeerTake when (peer == 0) { peer = 1; pw = 1; }
            action PeerDrop when (peer == 1) { peer = 0; pw = 1; }
            action PeerSense when (pw == 1) {
                pw = 0;
                sensed = if peer == ps { sensed } else { 1 };
                ps = peer;
            }
            // The timer's owed commit (`rows_deadline`): the words' want, no
            // hold standing, and nothing projecting.
            action Catch when (sensed == 0
                && (hand == 0 || hand == 2)
                && peer == 0
                && snap == hand
                && (snap_want > rows || rows > snap_want)) {
                sensed = 1;
            }

            invariant NoRegridUnderTheHand: grids_under_hand == 0;
            invariant OwnHandNeverRefusesTheFence: refused_own == 0;
            invariant NoBirthForAGoneHand: ghost == 0;
            invariant EveryMovedLeaseOwesAWake: snap == hand || woke == 1;
        }
    }
}

/// THE HELD ROW CATCHES UP: `[](hand = 0 => <>(rows = other))`, stated as
/// `[]<>(hand > 0 \/ peer > 0 \/ rows = other \/ gen >= GenCap)` — again and
/// again, either a hand holds the session or another the window shows
/// (`peer`), or the committed row is the one the live
/// facts want, or the bounded model's generation budget is spent (the one
/// horizon a finite model has). The hold [`driver_geometry_model`] adds lifts
/// only on a `Sense` after the lease moved, so this is the claim that the
/// hold never outlives the hand.
///
/// THE ENVIRONMENT IS NOT FAIR: a lease taken or let go, a turn's input
/// ending, mail, the program's output, the driver's read and fence — any of
/// them may happen or not. What the verdict ASSUMES, and nothing more:
///
/// * `Sense`, weakly fair — a posted wake is handled (the event loop runs;
///   `post_lease_changed` wakes it on every change of the lease);
/// * `Commit`, weakly fair — a refresh that has sensed commits its row;
/// * `Catch`, weakly fair — the timer wakes for a row count owed a commit
///   (`rows_deadline`). A held projection is dropped (`Turned`, not fair), and
///   a peer's lease let go before its wakes ran re-projects nothing, so
///   without it the row that lease held stands.
///
/// The mutant that breaks it alone: `DropNoWake`, a lease let go with no
/// wake — the hold then waits on a `Sense` that never comes, and the row a
/// gone hand's words raised stands.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn driver_geometry_liveness() -> super::Liveness {
    use super::{eq, gt, int, le, or_, var};
    super::Liveness {
        name: "TheHeldRowCatchesUp",
        goal: or_(
            or_(
                or_(gt(var("hand"), int(0)), gt(var("peer"), int(0))),
                eq(var("rows"), var("other")),
            ),
            le(var("GenCap"), var("gen")),
        ),
        weak: vec!["Sense", "Commit", "Catch"],
        strong: vec![],
        mutants: vec!["DropNoWake"],
    }
}
