// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AUTOPILOT'S TIMER: Cruise until a time, then Land
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3, §9.3.8).
//!
//! The derived twin of aterm-gui's `autopilot_timers` (the App's index, its
//! one deadline and the fire) and `session_timeline::fire_land_at` (the
//! compare-and-set on the stored pair). aterm-gui's
//! `autopilot_timers_conformance_tests` drive the real code against it.
//!
//! THE STORE is the session's pair as `SessionMeta` holds it — `word` (0
//! unset, 1 cruise, 2 land, 3 off) and `land_at` (0 none, else an instant on
//! the wall clock), written together. THE INDEX is the App's entry apart
//! from it — `idx` (0 none, else the instant it was indexed from) and `wait`,
//! how far its monotonic due lies ahead. `wake` is a wire write stored whose
//! `MetaChanged` the App has not applied; `wall` the wall clock (the
//! monotonic clock is `wait` counting down: `Tick` moves both, `Sleep` the
//! wall alone, `ClockBack` the wall back). `committed` is 0 in a seamless
//! update's successor before its Commit, which drops every `MetaChanged`;
//! `leaf`, `leaf_word` and `leaf_land` are the restore leaf that successor
//! grafts (the pair the outgoing process captured), and `bit` the autopilot
//! `driver_writes` bit of THIS process (a driver's write here, the fire's
//! own included, wins over a graft).
//!
//! Ghosts: `hand` (the last write stored was a hand write — a word with no
//! timer), `fires` (Land written by a fire since the last arm, capped at 2),
//! `over_hand` (a fire wrote Land over a stored hand write) and `early` (a
//! fire wrote Land with the wall short of the instant).
//!
//! Six invariants, each caught by a `Buggy` member:
//! `TheTimerGoesWithCruise` (`WireLandKeepsTimer`), `LandOncePerArm` and
//! `TheHandWins` (`TrustingFire`: a fire that trusts its index, which loses
//! to an in-flight Off and lands again at its next look;
//! `SameWordKeepsTimer` breaks `TheHandWins` too), `NeverEarly`
//! (`MonotonicFire`: the monotonic deadline alone, after a `ClockBack`),
//! `TheIndexTrailsTheStore` (`CommitWithoutRebuild`) and
//! `TheEntryGoesWithItsSession` (`CloseKeepsEntry`). The progress claim —
//! after the instant, with the pair still stored, Land is written — is
//! [`autopilot_timer_liveness`].
//!
//! Out of the model, as the design says (§9.3.7): an outgoing process that
//! fires after its restore leaf was captured, and the successor that lands
//! the carried timer again — the word ends `land` either way, and the
//! message log may say so twice. Each process writes Land at most once per
//! arm.

use aterm_spec::derive::{Liveness, Model, gt, int, or_, var};

/// The timer's world, bounded: a wall clock of `MaxWall` minutes, an arm
/// `Lead` minutes ahead (an `until=` lies strictly after now).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn autopilot_timer_model() -> Model {
    aterm_spec::ty_model! {
        AutopilotTimer {
            const Buggy = 0;
            const MaxWall = 3;
            const Lead = 2;
            var word = 0;
            var land_at = 0;
            var idx = 0;
            var wait = 0;
            var wake = 0;
            var wall = 0;
            var committed = 1;
            var alive = 1;
            var bit = 0;
            var leaf = 0;
            var leaf_word = 0;
            var leaf_land = 0;
            var hand = 0;
            var fires = 0;
            var over_hand = 0;
            var early = 0;

            // The window's writer (the menu, Settings): stored AND indexed.
            action SetCruiseUntil when (alive == 1 && committed == 1 && wall <= MaxWall - Lead) {
                word = 1;
                land_at = wall + Lead;
                idx = wall + Lead;
                wait = Lead;
                bit = 1;
                hand = 0;
                fires = 0;
            }
            // The wire: `meta set autopilot cruise until=…`, stored; its
            // `MetaChanged` not yet applied.
            action WireStoreUntil when (alive == 1 && wall <= MaxWall - Lead) {
                word = 1;
                land_at = wall + Lead;
                wake = if (word == 1 && land_at == wall + Lead) { wake } else { 1 };
                bit = 1;
                hand = 0;
                fires = 0;
            }
            // The wire's hand writes of the word, `until=` absent: each drops
            // the timer, the same word included. Every write marks the driver
            // bit; one that moves the pair posts its wake.
            action WireCruise when (alive == 1) {
                word = 1;
                land_at = 0;
                wake = if (word == 1 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action WireLand when (alive == 1) {
                word = 2;
                land_at = 0;
                wake = if (word == 2 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action WireOff when (alive == 1) {
                word = 3;
                land_at = 0;
                wake = if (word == 3 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            action Unset when (alive == 1) {
                word = 0;
                land_at = 0;
                wake = if (word == 0 && land_at == 0) { wake } else { 1 };
                bit = 1;
                hand = 1;
            }
            // `on_meta_changed`: the index re-reads the store — dropped in a
            // successor before its Commit.
            action ApplyWake when (alive == 1 && wake == 1) {
                idx = if (committed == 1) { land_at } else { idx };
                wait = if (committed == 1) {
                    if (land_at > wall) { land_at - wall } else { 0 }
                } else {
                    wait
                };
                wake = 0;
            }
            // Time: both clocks; the wall alone (a sleep, the clock set
            // forward); the wall set back.
            action Tick when (wall <= MaxWall - 1) {
                wall = wall + 1;
                wait = if (wait > 0) { wait - 1 } else { 0 };
            }
            action Sleep when (wall <= MaxWall - 1) {
                wall = wall + 1;
            }
            action ClockBack when (wall > 0) {
                wall = wall - 1;
            }
            // THE FIRE: the entry is due on the monotonic clock; taken out of
            // the index, and Land stored only where the STORED pair is the
            // entry's and the wall is at or past it (`fire_land_at`), marking
            // the driver bit. Short of it: indexed again from the remainder.
            // Moved: from the store. Never in a successor before its Commit.
            action Fire when (alive == 1 && committed == 1 && idx > 0 && wait == 0) {
                word = if (word == 1 && land_at == idx && idx <= wall) { 2 } else { word };
                land_at = if (word == 1 && land_at == idx && idx <= wall) { 0 } else { land_at };
                bit = if (word == 1 && land_at == idx && idx <= wall) { 1 } else { bit };
                fires = if (word == 1 && land_at == idx && idx <= wall) {
                    if (fires > 0) { 2 } else { 1 }
                } else {
                    fires
                };
                over_hand = if (word == 1 && land_at == idx && idx <= wall && hand == 1) {
                    1
                } else {
                    over_hand
                };
                idx = if (word == 1 && land_at == idx) {
                    if (idx <= wall) { 0 } else { idx }
                } else {
                    land_at
                };
                wait = if (word == 1 && land_at == idx) {
                    if (idx <= wall) { 0 } else { idx - wall }
                } else {
                    if (land_at > wall) { land_at - wall } else { 0 }
                };
            }
            // `Wake::WallMoved` (the watchdog saw a sleep), or a focus gain:
            // the due derived from the wall again.
            action WallMoved when (alive == 1 && idx > 0) {
                wait = if (idx > wall) { idx - wall } else { 0 };
            }
            // aterm quits and comes back (a cold restore): the leaf carries the
            // pair, the new process seeds and indexes it; a pending wake and
            // the driver bit die with the old process.
            action CloseAndRestore when (alive == 1 && committed == 1) {
                idx = land_at;
                wait = if (land_at > wall) { land_at - wall } else { 0 };
                wake = 0;
                bit = 0;
            }
            // A seamless update: the successor adopts the session with a fresh
            // store and owes it the leaf's graft; its index is empty.
            action HandoffStart when (alive == 1 && committed == 1) {
                committed = 0;
                leaf = 1;
                leaf_word = word;
                leaf_land = land_at;
                word = 0;
                land_at = 0;
                idx = 0;
                wait = 0;
                wake = 0;
                bit = 0;
            }
            // The live graft (`restore_carried_meta`): the pair as one step,
            // unless a driver wrote the word here first; then indexed.
            action Graft when (alive == 1 && committed == 0 && leaf == 1 && bit == 0) {
                word = leaf_word;
                land_at = leaf_land;
                idx = leaf_land;
                wait = if (leaf_land > wall) { leaf_land - wall } else { 0 };
            }
            // The Commit: the index rebuilt from the store.
            action HandoffCommit when (committed == 0) {
                committed = 1;
                leaf = 0;
                leaf_word = 0;
                leaf_land = 0;
                idx = if (alive == 1) { land_at } else { 0 };
                wait = if (alive == 1 && land_at > wall) { land_at - wall } else { 0 };
                wake = 0;
            }
            // The session closes: its store and its entry go with it.
            action CloseSession when (alive == 1) {
                alive = 0;
                word = 0;
                land_at = 0;
                bit = 0;
                idx = 0;
                wait = 0;
                wake = 0;
            }

            // Mutants (dead at Buggy = 0).
            // A fire that trusts the index: Land wherever the entry is due by
            // its own instant, the store unread, the entry left in — it loses
            // to an in-flight Off, and lands again at its next look.
            action TrustingFire when (Buggy == 1 && alive == 1 && committed == 1 && idx > 0 && wait == 0 && idx <= wall) {
                word = 2;
                land_at = 0;
                bit = 1;
                over_hand = if (hand == 1) { 1 } else { over_hand };
                fires = if (fires > 0) { 2 } else { 1 };
            }
            // A same-word write that keeps the timer.
            action SameWordKeepsTimer when (Buggy == 1 && alive == 1 && word == 1 && land_at > 0) {
                bit = 1;
                hand = 1;
            }
            // A fire on the monotonic deadline alone, the wall unread.
            action MonotonicFire when (Buggy == 1 && alive == 1 && committed == 1 && idx > 0 && wait == 0 && word == 1 && land_at == idx) {
                word = 2;
                land_at = 0;
                idx = 0;
                bit = 1;
                early = if (idx > wall) { 1 } else { early };
                fires = if (fires > 0) { 2 } else { 1 };
            }
            // A Land written by hand that keeps the timer.
            action WireLandKeepsTimer when (Buggy == 1 && alive == 1 && land_at > 0) {
                word = 2;
                wake = 1;
                bit = 1;
                hand = 1;
            }
            // A Commit that does not rebuild the index.
            action CommitWithoutRebuild when (Buggy == 1 && committed == 0) {
                committed = 1;
                leaf = 0;
                leaf_word = 0;
                leaf_land = 0;
                wake = 0;
            }
            // A close that leaves the session's entry behind.
            action CloseKeepsEntry when (Buggy == 1 && alive == 1 && idx > 0) {
                alive = 0;
                word = 0;
                land_at = 0;
                bit = 0;
                wake = 0;
            }

            invariant TheTimerGoesWithCruise: land_at == 0 || word == 1;
            invariant LandOncePerArm: fires <= 1;
            invariant TheHandWins: over_hand == 0;
            invariant NeverEarly: early == 0;
            invariant TheIndexTrailsTheStore: alive == 0 || committed == 0 || wake == 1 || idx == land_at;
            invariant TheEntryGoesWithItsSession: alive == 1 || idx == 0;
        }
    }
}

/// AFTER THE INSTANT, LAND IS WRITTEN (`[]<>`): the goal — the session gone,
/// no timer stored, or the wall still short of it — keeps coming back, so a
/// stored timer whose instant came is landed. Each fairness is load-bearing:
/// the fire (`Fire`), the wall's re-derivation after a sleep (`WallMoved`),
/// the App applying a wire write's wake (`ApplyWake`) and a successor's
/// Commit (`HandoffCommit`). `CommitWithoutRebuild` breaks it alone: a timer
/// armed over the wire before the Commit is never indexed.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(autopilot_timer_model)]
pub fn autopilot_timer_liveness() -> Liveness {
    Liveness {
        name: "TheTimeLands",
        goal: or_(
            or_(gt(int(1), var("alive")), gt(int(1), var("land_at"))),
            gt(var("land_at"), var("wall")),
        ),
        weak: vec!["Fire", "WallMoved", "ApplyWake", "HandoffCommit"],
        strong: vec![],
        mutants: vec!["CommitWithoutRebuild"],
    }
}
