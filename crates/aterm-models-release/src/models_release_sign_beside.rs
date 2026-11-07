// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Steps 7 and 8 of a release once it is ready: R's source published by `pub publish` and R's
//! verified bundle signed, notarized and packaged privately by `ship cut --sign`, at once, then
//! the cut's claim, which adopts that copy, and the binary made public (B13 of
//! `docs/DESIGN-fast-build-release-2026-11-05.md`).
//!
//! # Variables
//! - `phase`: 0 ready (step 6 passed), 1 step 7 runs, 2 step 7 passed, 3 claimed (step 8's
//!   ledger claim landed), 4 cut (the binary is public), 5 refused, 6 lost (the claim lost the
//!   ledger's race).
//! - `source`: step 7's source side. 0 idle, 1 running, 2 passed, 3 red, 4 could not run,
//!   5 stopped (on the sign's red, before its push), 6 skipped (published by an earlier run),
//!   7 pushing (its pre-promote hook took the gate; a push is never stopped).
//! - `sign`: step 7's sign side. 0 idle, 1 running, 2 passed, 3 red, 4 could not run, 5 stopped
//!   (on the source's red).
//! - `gate`: the gate between the source's push and the sign's red. 0 open, 1 push (the hook took
//!   it), 2 stop (the release took it on the sign's red).
//! - `red`: 1 once a side ended red.
//! - `record`: 1 while the signed copy and its record stand in `dist/`.
//! - `src_public`, `bin_public`: 1 once R's source, R's binary is public.
//! - `claimed`, `lost`: 1 once the claim landed, lost its race.
//!
//! # Actions
//! Ready (phase 0): `SkipSource` (an earlier run published it), then `Begin` (to 1).
//! Step 7 (phase 1): `StartSource`, `StartSign`, neither after a red. `SourcePush` takes an open
//! gate; `SourcePass` from pushing; `SourceRed` and `SourceCouldNotRun` from pushing, or from
//! running while the gate is not stopped; `StopSource` once the gate is stopped (running, or never
//! started). `SignPass` keeps the record; `SignRed` stops an open gate; `SignCouldNotRun`;
//! `StopSign` on a red. `PublishPass` once the source passed (or was skipped) and the sign passed
//! (to 2); `PublishRefuse` once no side runs and step 7 cannot pass (to 5).
//! Step 8 (phase 2): `Claim` (to 3); `LoseRace` discards the copy (to 6); `ClaimRefused` (another
//! pre-claim gate; the copy is kept for the next run) (to 5). `CutPublish` makes the binary
//! public (to 4); `CutRefuse` (to 5, claimed: a resume finishes it).
//!
//! # Mutants
//! `Buggy == k` enables the one action written for dial k, and each breaks the law after it:
//! 1 `ClaimBeforeTheSign` (`ClaimOnlyAfterSignAndPublish`), 2 `SignPublishes`
//! (`NothingPublicBeforePublish`), 3 `LostRaceKeepsTheCopy` (`LostRaceMakesNothingPublic`),
//! 4 `HookIgnoresTheGate` (`NoPushAfterTheSignsRed`), 5 `StopAPushInFlight`
//! (`PushInFlightFinishes`), 6 `RefuseBeforeTheSidesEnd` (`NoSideOutlivesTheStep`),
//! 7 `StopOnACouldNotRun` (`StopOnlyOnARed`), 8 `UploadBeforeTheClaim`
//! (`BinaryOnlyAfterTheClaim`).

use aterm_spec::derive::*;

/// Step 7's two sides and step 8's claim of the copy the sign side made.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn release_sign_beside_publish_model() -> Model {
    aterm_spec::ty_model! {
        ReleaseSignBesidePublish {
            const Buggy = 0;
            var phase = 0;
            var source = 0;
            var sign = 0;
            var gate = 0;
            var red = 0;
            var record = 0;
            var src_public = 0;
            var bin_public = 0;
            var claimed = 0;
            var lost = 0;

            action SkipSource when (phase == 0 && source == 0) {
                source = 6;
                src_public = 1;
            }
            action Begin when (phase == 0) {
                phase = 1;
            }

            action StartSource when (phase == 1 && source == 0 && red == 0) {
                source = 1;
            }
            action StartSign when (phase == 1 && sign == 0 && red == 0) {
                sign = 1;
            }

            action SourcePush when (source == 1 && gate == 0) {
                source = 7;
                gate = 1;
                src_public = 1;
            }
            action SourcePass when (source == 7) {
                source = 2;
            }
            action SourceRed when (source == 7 || (source == 1 && gate == 0)) {
                source = 3;
                red = 1;
            }
            action SourceCouldNotRun when (source == 7 || (source == 1 && gate == 0)) {
                source = 4;
            }
            action StopSource when (phase == 1 && source <= 1 && gate == 2) {
                source = 5;
            }

            action SignPass when (sign == 1 && red == 0) {
                sign = 2;
                record = 1;
            }
            action SignRed when (sign == 1 && red == 0) {
                sign = 3;
                red = 1;
                gate = if gate == 0 { 2 } else { gate };
            }
            action SignCouldNotRun when (sign == 1 && red == 0) {
                sign = 4;
            }
            action StopSign when (phase == 1 && sign <= 1 && red == 1) {
                sign = 5;
            }

            action PublishPass when (phase == 1 && (source == 2 || source == 6) && sign == 2) {
                phase = 2;
            }
            action PublishRefuse when (
                phase == 1 && source > 1 && source <= 6 && sign > 1 &&
                    (source == 3 || source == 4 || source == 5 || sign > 2)
            ) {
                phase = 5;
            }

            action Claim when (phase == 2) {
                phase = 3;
                claimed = 1;
            }
            action LoseRace when (phase == 2) {
                phase = 6;
                lost = 1;
                record = 0;
            }
            action ClaimRefused when (phase == 2) {
                phase = 5;
            }
            action CutPublish when (phase == 3) {
                phase = 4;
                bin_public = 1;
            }
            action CutRefuse when (phase == 3) {
                phase = 5;
            }

            action ClaimBeforeTheSign when (
                Buggy == 1 && phase == 1 && (source == 2 || source == 6) && sign == 1
            ) {
                phase = 3;
                claimed = 1;
            }
            action SignPublishes when (Buggy == 2 && sign == 1 && src_public == 0) {
                bin_public = 1;
            }
            action LostRaceKeepsTheCopy when (Buggy == 3 && phase == 2) {
                phase = 6;
                lost = 1;
            }
            action HookIgnoresTheGate when (Buggy == 4 && source == 1 && gate == 2) {
                source = 7;
                src_public = 1;
            }
            action StopAPushInFlight when (Buggy == 5 && source == 7 && red == 1) {
                source = 5;
            }
            action RefuseBeforeTheSidesEnd when (
                Buggy == 6 && phase == 1 && red == 1 && (source == 1 || source == 7 || sign == 1)
            ) {
                phase = 5;
            }
            action StopOnACouldNotRun when (Buggy == 7 && sign == 1 && red == 0 && source == 4) {
                sign = 5;
            }
            action UploadBeforeTheClaim when (Buggy == 8 && phase == 2 && bin_public == 0) {
                bin_public = 1;
            }

            invariant ClaimOnlyAfterSignAndPublish:
                claimed == 0 || ((source == 2 || source == 6) && sign == 2);
            invariant NothingPublicBeforePublish:
                bin_public == 0 || (src_public == 1 && (source == 2 || source == 6));
            invariant BinaryOnlyAfterTheClaim: bin_public == 0 || claimed == 1;
            invariant LostRaceMakesNothingPublic:
                lost == 0 || (bin_public == 0 && record == 0 && claimed == 0);
            invariant NoPushAfterTheSignsRed:
                gate <= 1 || source <= 1 || (source > 2 && source <= 6);
            invariant PushInFlightFinishes: gate == 0 || gate > 1 || source <= 4 || source > 5;
            invariant NoSideOutlivesTheStep:
                phase <= 1 ||
                    ((source == 0 || (source > 1 && source <= 6)) && (sign == 0 || sign > 1));
            invariant StopOnlyOnARed: red == 1 || ((source <= 4 || source > 5) && sign <= 4);
            bound SignBesideBox:
                phase <= 6 && source <= 7 && sign <= 5 && gate <= 2 && red <= 1 &&
                    record <= 1 && src_public <= 1 && bin_public <= 1 && claimed <= 1 &&
                    lost <= 1;
        }
    }
}
