// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A published index's cross-process spacing decision, shared by the window
//! before spawn and an atpkg child after it waited for the store lock.

use super::*;

/// The pass-end writer binds a requested or resolved index to the outcome and
/// end time in one durable record. `target` is 0 unknown, 1 old build 44, or 2
/// current build 45. `current` says that witness belongs to the latest attempt;
/// older schemas and a later unrelated attempt fail closed. Both readers run
/// for a strictly newer 45 only with that witness, no live holder and no
/// metered hold. `Buggy=1` reverses the run/wait verdict at the shared reader
/// boundary: it catches both the old blanket wait for a fresh higher target
/// and unsafe bypasses of each spacing guard. `Buggy=2` bypasses every target
/// independently (catches duplicate failed 45).
/// Tier-1 drives the real status writer/parser, GUI gate and queued CLI child.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_published_spacing_model() -> Model {
    crate::ty_model! {
        AtpkgPublishedSpacing {
            const Buggy = 0;
            var recorded = 0;
            var target = 0;
            // 0 unknown, 1 ok, 2 failed.
            var outcome = 0;
            var current = 0;
            var in_flight = 0;
            var metered_hold = 0;
            var published_45 = 0;
            // 0 undecided, 1 run, 2 wait.
            var gui = 0;
            var queued = 0;

            action EndOldOk when (recorded == 0 && gui == 0 && queued == 0) {
                recorded = 1;
                target = 1;
                outcome = 1;
                current = 1;
            }
            action EndOldFailed when (recorded == 0 && gui == 0 && queued == 0) {
                recorded = 1;
                target = 1;
                outcome = 2;
                current = 1;
            }
            action EndSameOk when (recorded == 0 && gui == 0 && queued == 0) {
                recorded = 1;
                target = 2;
                outcome = 1;
                current = 1;
            }
            action EndSameFailed when (recorded == 0 && gui == 0 && queued == 0) {
                recorded = 1;
                target = 2;
                outcome = 2;
                current = 1;
            }
            action EndUnknown when (recorded == 0 && gui == 0 && queued == 0) {
                recorded = 1;
                outcome = 1;
            }
            action StaleWitness when (
                recorded == 1 && current == 1 && gui == 0 && queued == 0
            ) {
                current = 0;
            }
            action LiveHolder when (in_flight == 0 && gui == 0 && queued == 0) {
                in_flight = 1;
            }
            action RateHold when (metered_hold == 0 && gui == 0 && queued == 0) {
                metered_hold = 1;
            }
            action Publish45 when (published_45 == 0) {
                published_45 = 1;
            }
            action DecideGui when (published_45 == 1 && gui == 0) {
                gui = if target == 1 && outcome > 0 && current == 1 &&
                    in_flight == 0 && metered_hold == 0 {
                    if Buggy == 1 { 2 } else { 1 }
                } else {
                    if Buggy == 1 || Buggy == 2 { 1 } else { 2 }
                };
            }
            action DecideQueued when (published_45 == 1 && queued == 0) {
                queued = if target == 1 && outcome > 0 && current == 1 &&
                    in_flight == 0 && metered_hold == 0 {
                    if Buggy == 1 { 2 } else { 1 }
                } else {
                    if Buggy == 1 || Buggy == 2 { 1 } else { 2 }
                };
            }

            invariant FreshHigherRuns:
                target == 0 || target == 2 || outcome == 0 || current == 0 ||
                in_flight == 1 || metered_hold == 1 ||
                ((gui == 0 || gui == 1) && (queued == 0 || queued == 1));
            invariant SameTargetWaits:
                target == 0 || target == 1 ||
                ((gui == 0 || gui == 2) && (queued == 0 || queued == 2));
            invariant UnknownTargetWaits:
                target == 1 || target == 2 ||
                ((gui == 0 || gui == 2) && (queued == 0 || queued == 2));
            invariant LiveHolderWaits:
                in_flight == 0 ||
                ((gui == 0 || gui == 2) && (queued == 0 || queued == 2));
            invariant MeteredHoldWaits:
                metered_hold == 0 ||
                ((gui == 0 || gui == 2) && (queued == 0 || queued == 2));
            invariant StaleWitnessWaits:
                current == 1 ||
                ((gui == 0 || gui == 2) && (queued == 0 || queued == 2));
        }
    }
}
