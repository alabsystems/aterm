// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where an update's successor rebuilds its layout (the parallel attach;
//! `docs/DESIGN-warm-successor-2026-09-29.md`). The rebuild is
//! `App::apply_pending_restore`; where it runs is `restore_pass_due`. A cold launch runs it
//! at a park after the first present, so window 0 paints its shell before N shell forks.
//! On the handoff lane every window stays hidden until its own first present and the
//! adoption proof waits for all of them, so the pass runs in window 0's `resumed`: windows
//! 1..N do not wait a second time for a hidden window's first drawable (about 30 ms after
//! the turn that created it).
//!
//! A cold rebuild runs in slices, one per park (`restore_pass_sliced`), with input and
//! presents between them, so a window that has painted is not frozen for every fork. A
//! rebuild that places handed-off shells (a handoff, proving or degraded) runs whole.

use super::{Liveness, Model, and_, eq, int, or_, var};

/// The successor's layout rebuild and its adoption proof, as `restore_pass_due`,
/// `restore_pass_sliced` and `App::maybe_signal_handoff_ready` decide them.
///
/// State: `lane` — 0 not started, 1 an update's successor that can still prove, 2 a cold
/// launch (or a degraded handoff, which never proves). `handed` — the rebuild places
/// handed-off shells (a handoff, proving or degraded). `draining` — a cold rebuild has run
/// a slice and owes the rest. `many` — the layout has windows beyond window 0. `phase` — 0
/// before `resumed`, 1 inside the `resumed` that attached window 0, 2 at the parks after
/// it. `queued` — the rebuild is still owed. `extras` — windows 1..N exist. `kicked` — the
/// rebuild started each extra window's direct first present (a hidden macOS window is not
/// reliably sent `RedrawRequested`). `painted0` / `painted_extras` — window 0 / every
/// extra window has presented. `proved` — the adoption proof is written.
///
/// Invariants, each caught by its own `Buggy = 1` action:
/// * `ColdRestoreAfterFirstPaint` — a cold launch never rebuilds before a window has
///   presented; broken by `RebuildAtAttachCold`.
/// * `HandoffAttachesInOnePass` — the handoff lane never leaves window 0's attach with the
///   rebuild owed; broken by `LeaveResumeOwing`.
/// * `ProofNeedsDrainedAndPainted` — the proof needs the rebuild drained and every window
///   painted; broken by `ProveAtFirstPresent` (the parent would exit while windows 2..N
///   have no replacement).
/// * `EveryRebuiltWindowPresents` — broken by `RebuildWithoutKick`, which is also the
///   liveness mutant: extras never present, so the proof never comes.
/// * `HandedRebuildsWhole` — a rebuild that places handed-off shells never runs in
///   slices; broken by `RebuildSliceHanded`, which slices a degraded handoff's.
/// * `ColdSlicesAfterFirstPaint` — no slice runs before a window has presented (a slice
///   leaves the rebuild owed, so `ColdRestoreAfterFirstPaint` does not see it); broken by
///   `RebuildSliceBeforePaint`.
///
/// `extras` turns only at a drain's last slice, though the real slices create windows
/// earlier: the proof is what waits on them, and a cold launch never proves.
///
/// Liveness (`TheSuccessorProves`, [`native_update_successor_attach_liveness`]) holds under
/// weak fairness of the app's steps and the window server's first drawables.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_successor_attach_model() -> Model {
    crate::ty_model! {
        NativeUpdateSuccessorAttach {
            const Buggy = 0;
            var lane = 0;
            var many = 0;
            var phase = 0;
            var queued = 1;
            var extras = 0;
            var kicked = 0;
            var painted0 = 0;
            var painted_extras = 0;
            var proved = 0;
            var handed = 0;
            var draining = 0;
            // The layout the boot restores has windows beyond window 0.
            action Widen when (lane == 0 && many == 0) {
                many = 1;
            }
            action StartHandoff when (lane == 0) {
                lane = 1;
                handed = 1;
            }
            action StartCold when (lane == 0) {
                lane = 2;
            }
            // A handoff that degraded before its rebuild: the cold lane's
            // order, with handed-off shells to place.
            action StartDegraded when (lane == 0) {
                lane = 2;
                handed = 1;
            }
            // `resumed`: window 0 attached hidden, its direct first present
            // started.
            action Attach0 when (lane > 0 && phase == 0) {
                phase = 1;
            }
            // `restore_pass_due(FirstAttach, ..)`.
            action RebuildAtAttach when (phase == 1 && queued == 1 && lane == 1) {
                queued = 0;
                extras = many;
                kicked = many;
            }
            // `resumed` returns: the handoff lane only once its rebuild ran.
            action LeaveResume when (phase == 1 && (queued == 0 || lane == 2)) {
                phase = 2;
            }
            // `restore_pass_due(Park, ..)`: after a window has presented. The
            // whole pass, or a drain's last slice.
            action RebuildAtPark when (
                phase == 2 && queued == 1 && painted0 + painted_extras > 0
            ) {
                queued = 0;
                extras = many;
                kicked = many;
                draining = 0;
            }
            // `restore_pass_sliced(Park, ..)`: a slice that leaves the rest of
            // a cold rebuild owed to the next park.
            action RebuildSliceAtPark when (
                phase == 2 && queued == 1 && painted0 + painted_extras > 0 && handed == 0
            ) {
                draining = 1;
            }
            // The window server hands a hidden window its first drawable.
            action Present0 when (phase > 0 && painted0 == 0) {
                painted0 = 1;
            }
            action PresentExtras when (extras == 1 && kicked == 1 && painted_extras == 0) {
                painted_extras = 1;
            }
            // `maybe_signal_handoff_ready`.
            action Prove when (
                lane == 1 && proved == 0 && painted0 == 1 &&
                queued == 0 && painted_extras == extras
            ) {
                proved = 1;
            }
            // Mutant: the attach pass on the cold lane too.
            action RebuildAtAttachCold when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 2
            ) {
                queued = 0;
                extras = many;
                kicked = many;
            }
            // Mutant: the handoff lane leaves `resumed` with its rebuild owed.
            action LeaveResumeOwing when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 1
            ) {
                phase = 2;
            }
            // Mutant: the proof at window 0's first present alone.
            action ProveAtFirstPresent when (
                Buggy == 1 && lane == 1 && proved == 0 && painted0 == 1
            ) {
                proved = 1;
            }
            // Mutant: a degraded handoff's rebuild in slices.
            action RebuildSliceHanded when (
                Buggy == 1 && phase == 2 && queued == 1 &&
                painted0 + painted_extras > 0 && handed == 1
            ) {
                draining = 1;
            }
            // Mutant: a cold slice before any window has presented.
            action RebuildSliceBeforePaint when (
                Buggy == 1 && phase == 1 && queued == 1 && handed == 0 &&
                painted0 + painted_extras == 0
            ) {
                draining = 1;
            }
            // Mutant (liveness): the rebuild creates the extra windows but starts none of
            // their first presents.
            action RebuildWithoutKick when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 1 && many == 1
            ) {
                queued = 0;
                extras = 1;
                kicked = 0;
            }
            invariant ColdRestoreAfterFirstPaint:
                lane == 1 || queued == 1 || painted0 + painted_extras > 0;
            invariant HandoffAttachesInOnePass:
                lane == 2 || phase <= 1 || queued == 0;
            invariant EveryRebuiltWindowPresents: kicked == extras;
            invariant ProofNeedsDrainedAndPainted:
                proved == 0 || (queued == 0 && painted0 == 1 && painted_extras == extras);
            invariant HandedRebuildsWhole: handed == 0 || draining == 0;
            invariant ColdSlicesAfterFirstPaint:
                draining == 0 || painted0 + painted_extras > 0;
        }
    }
}

/// [`native_update_successor_attach_model`]'s progress claim: every handoff
/// successor proves, and every cold launch ends rebuilt with its windows
/// painted.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[crate::liveness(crate::derive::native_update_successor_attach_model)]
pub fn native_update_successor_attach_liveness() -> Liveness {
    Liveness {
        name: "TheSuccessorProves",
        goal: or_(
            eq(var("proved"), int(1)),
            and_(
                eq(var("lane"), int(2)),
                and_(
                    eq(var("queued"), int(0)),
                    and_(
                        eq(var("painted0"), int(1)),
                        eq(var("painted_extras"), var("extras")),
                    ),
                ),
            ),
        ),
        weak: vec![
            "StartHandoff",
            "Attach0",
            "RebuildAtAttach",
            "LeaveResume",
            "RebuildAtPark",
            "Present0",
            "PresentExtras",
            "Prove",
        ],
        strong: vec![],
        mutants: vec!["RebuildWithoutKick"],
    }
}
