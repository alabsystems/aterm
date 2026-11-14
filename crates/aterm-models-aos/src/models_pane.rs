// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The formal model `AosPaneFaithfulReplay` of Rewind R3's pane task and its contract
//! `pane.faithful-replay` v1, over the check `aterm_aos_pane::trace::PaneRecording::check` makes
//! of one chain.

use aterm_spec::derive::Model;

/// One recorded chain of a pane, input by input, and the verdict a check of the chain so far
/// reaches: the replay of `PaneCode::ThisBuild` from the chain's checkpoint, its replies shadowed
/// by the recorded ones, judged by `pane.faithful-replay`.
///
/// Variables: `n` is the inputs recorded so far, the cursor of the next one. `z` and `w` count
/// the resizes and live witnesses among them, and `m` the witnesses whose replayed screen was the
/// live one (`PaneCounts`). `div` is 1 once the replayed screen differs from the live one. The
/// ghost `first` is the first fault the replay reached: 1 a witness that is not the replayed
/// screen, 2 a slice whose recorded reply is not the one the replay makes, 3 a slice that carries
/// no clock; `fat` is its cursor. `verdict` is the check's: 1 `pass`, 2 `fail:WitnessMatches`,
/// 3 `fail:RepliesMatch`, 4 `fail:ClockRecorded`, and 0 for no verdict, which only a dial
/// reaches; `at` is the cursor a fail names.
///
/// Actions, each one more input: `Slice` (a clocked slice whose recorded reply is the live
/// engine's), `Resize` and `Witness` (the live screen's digest) are what a faithful recorder
/// writes. The faults: `SkewedSlice` records a slice under another clock than the one the live
/// engine parsed it at, and its bytes show the clock (a prompt mark), so the replayed screen
/// differs from then on; `AlteredReply` records a reply the replay does not make (an altered DA
/// reply); `DroppedClock` records a slice with no clock; `ForgedWitness` records a digest that is
/// no screen of the chain. A law that fails does not stop the replay.
///
/// A chain with no fault passes, with or without a witness, and the replay has no work budget.
///
/// The model covers chains of slices, resizes and witnesses whose every step the engine admits.
/// The check also reaches these verdicts from inputs outside it: `ClockRecorded` for a host
/// operation with no clock, `WitnessMatches` for a refused step, a short run or an observation of
/// another kind than its input, and `RepliesMatch` for recorded replies the replay never reached.
///
/// Dials, each of which alone breaks its own law: 1 a witness that differs from the replayed
/// screen is judged a match. 2 a reply that diverged is not judged. 3 a slice with no clock is
/// not judged. 4 a witness that matched is taken for no verdict.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn aos_pane_faithful_replay_model() -> Model {
    aterm_spec::ty_model! {
        AosPaneFaithfulReplay {
            const Buggy = 0;
            const MaxIn = 4;
            var n = 0;
            var z = 0;
            var w = 0;
            var m = 0;
            var div = 0;
            var first = 0;
            var fat = 0;
            var verdict = 1;
            var at = 0;

            action Slice when (n + 1 <= MaxIn) {
                n = n + 1;
            }
            action Resize when (n + 1 <= MaxIn) {
                n = n + 1;
                z = z + 1;
            }
            action Witness when (n + 1 <= MaxIn) {
                n = n + 1;
                w = w + 1;
                m = if div == 0 { m + 1 } else { m };
                first = if first > 0 || div == 0 { first } else { 1 };
                fat = if first > 0 || div == 0 { fat } else { n };
                verdict = if verdict > 1 {
                    verdict
                } else if div == 1 {
                    if Buggy == 1 { verdict } else { 2 }
                } else if Buggy == 4 {
                    0
                } else {
                    verdict
                };
                at = if verdict > 1 || div == 0 || Buggy == 1 { at } else { n };
            }
            action ForgedWitness when (n + 1 <= MaxIn) {
                n = n + 1;
                w = w + 1;
                first = if first > 0 { first } else { 1 };
                fat = if first > 0 { fat } else { n };
                verdict = if verdict > 1 || Buggy == 1 { verdict } else { 2 };
                at = if verdict > 1 || Buggy == 1 { at } else { n };
            }
            action SkewedSlice when (n + 1 <= MaxIn) {
                n = n + 1;
                div = 1;
            }
            action AlteredReply when (n + 1 <= MaxIn) {
                n = n + 1;
                first = if first > 0 { first } else { 2 };
                fat = if first > 0 { fat } else { n };
                verdict = if verdict > 1 || Buggy == 2 { verdict } else { 3 };
                at = if verdict > 1 || Buggy == 2 { at } else { n };
            }
            action DroppedClock when (n + 1 <= MaxIn) {
                n = n + 1;
                first = if first > 0 { first } else { 3 };
                fat = if first > 0 { fat } else { n };
                verdict = if verdict > 1 || Buggy == 3 { verdict } else { 4 };
                at = if verdict > 1 || Buggy == 3 { at } else { n };
            }

            invariant WitnessMatches: if first == 1 { verdict == 2 && at == fat } else { 1 == 1 };
            invariant RepliesMatch: if first == 2 { verdict == 3 && at == fat } else { 1 == 1 };
            invariant ClockRecorded: if first == 3 { verdict == 4 && at == fat } else { 1 == 1 };
            invariant FaithfulPasses: if first == 0 { verdict == 1 && at == 0 } else { 1 == 1 };
            bound InputsBounded: n <= MaxIn;
            bound ResizesBounded: z <= MaxIn;
            bound WitnessesBounded: w <= MaxIn && m <= MaxIn;
        }
    }
}
