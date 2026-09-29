// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE AN UPDATE'S SUCCESSOR REBUILDS ITS LAYOUT (warm successor P3, the
//! parallel attach, 2026-09-29; `docs/DESIGN-warm-successor-2026-09-29.md`).
//!
//! A launch that restores a layout attaches window 0 in `resumed` and rebuilds
//! the rest (windows 1..N, their tabs, the handed-off shells) in one pass,
//! `App::apply_pending_restore`. A COLD launch keeps RESTORE-1: the pass waits
//! for the first present and runs at a park, so window 0 paints its shell
//! before N shell forks. On the HANDOFF lane every window stays hidden until
//! its own first present, and the adoption proof waits for all of them, so the
//! pass runs in the same `resumed` as window 0's attach. A hidden window's
//! first drawable arrives about 30 ms after the event-loop turn that created
//! it, so windows 1..N no longer pay that wait a second time behind window 0.

use super::{Liveness, Model, and_, eq, int, or_, var};

/// The successor's layout rebuild and its adoption proof, as
/// `aterm-gui`'s `restore_pass_due` and `App::maybe_signal_handoff_ready`
/// decide them.
///
/// State: `lane` — 0 not started, 1 an update's successor that can still
/// prove, 2 a cold launch (or a degraded handoff, which never proves).
/// `many` — the layout has windows beyond window 0. `phase` — 0 before
/// `resumed`, 1 inside the `resumed` that attached window 0 and started its
/// first present, 2 at the parks after it. `queued` — the rebuild is still
/// owed. `extras` — windows 1..N exist. `kicked` — the rebuild started each
/// extra window's direct first present (a hidden macOS window is not reliably
/// sent `RedrawRequested`). `painted0` / `painted_extras` — window 0 / every
/// extra window has presented. `proved` — the adoption proof is written.
///
/// Invariants, each caught by its own `Buggy = 1` action, so none masks
/// another: `ColdRestoreAfterFirstPaint` — a cold launch never rebuilds
/// before a window has presented (RESTORE-1); `RebuildAtAttachCold` runs the
/// attach pass on the cold lane. `HandoffAttachesInOnePass` — the handoff
/// lane never leaves window 0's attach with the rebuild still owed;
/// `LeaveResumeOwing` waits for the park, as before P3.
/// `ProofNeedsDrainedAndPainted` — the proof needs the rebuild drained and
/// every window, window 0 and each extra, painted; `ProveAtFirstPresent`
/// proves at window 0's first present alone (the multi-window trap: the parent
/// exits while windows 2..N have no replacement).
///
/// `EveryRebuiltWindowPresents` — the rebuild starts the first present of
/// every extra window it creates; `RebuildWithoutKick` creates them without.
///
/// Liveness (`TheSuccessorProves`, [`native_update_successor_attach_liveness`]):
/// a handoff proves, and a cold launch ends rebuilt and painted, under weak
/// fairness of the app's steps and of the window server's first drawables.
/// Mutant: `RebuildWithoutKick`, extra windows created without their direct
/// first present, which never present and so never let the proof through.
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
            // The layout the boot restores has windows beyond window 0.
            action Widen when (lane == 0 && many == 0) {
                many = 1;
            }
            action StartHandoff when (lane == 0) {
                lane = 1;
            }
            action StartCold when (lane == 0) {
                lane = 2;
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
            // `restore_pass_due(Park, ..)`: after a window has presented.
            action RebuildAtPark when (
                phase == 2 && queued == 1 && painted0 + painted_extras > 0
            ) {
                queued = 0;
                extras = many;
                kicked = many;
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
            // MUTANT: the attach pass on the cold lane too.
            action RebuildAtAttachCold when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 2
            ) {
                queued = 0;
                extras = many;
                kicked = many;
            }
            // MUTANT: the handoff lane leaves `resumed` with its rebuild owed.
            action LeaveResumeOwing when (
                Buggy == 1 && phase == 1 && queued == 1 && lane == 1
            ) {
                phase = 2;
            }
            // MUTANT: the proof at window 0's first present alone.
            action ProveAtFirstPresent when (
                Buggy == 1 && lane == 1 && proved == 0 && painted0 == 1
            ) {
                proved = 1;
            }
            // MUTANT (liveness): the rebuild creates the extra windows but
            // starts none of their first presents.
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
        }
    }
}

/// [`native_update_successor_attach_model`]'s progress claim: every handoff
/// successor proves, and every cold launch ends rebuilt with its windows
/// painted.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
