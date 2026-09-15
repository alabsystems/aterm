// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-release custody for an existing ribbon, without new typing.

use super::Model;

/// One two-cell ribbon was active before a content observation released it.
/// `clock=1` names its exact saved pre-release clock tuple; `clock=2` is the
/// abandoned/retracting state. These are identities, not elapsed durations.
/// A full observation includes every original (row, column, birth, glyph,
/// width) unit and at least one returned released nonblank unit. Partial,
/// missing, changed and unsampled observations cannot supply that authority.
///
/// Explicit abandonment, replacement, or retirement revokes restoration
/// custody. Failed observations alone retain it. A successful restoration
/// consumes custody once and restores the old clock without minting cells or
/// births. Ordinary later typed input and animation ticks are outside this
/// bounded transaction. Tier-1 drives the real witness and ribbon, projecting
/// abandonment, exact clock equality, cell count and original birth identity.
/// `Buggy=1` omits the full-observation requirement, allowing partial content
/// to revive an abandoned ribbon.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ribbon_release_restoration_model() -> Model {
    crate::ty_model! {
        RibbonReleaseRestoration {
            const Buggy = 0;
            // 0 active, 1 released, 2 restored.
            var phase = 0;
            var custody = 0;
            var revoked = 0;
            var originals = 1;
            var cells = 2;
            var births = 2;
            var clock = 1;
            // 0 unsampled, 1 partial, 2 full exact return, 3 missing, 4 changed.
            var sample = 0;

            action Release when (phase == 0) {
                phase = 1; custody = if revoked == 0 { 1 } else { 0 };
                clock = 2; sample = 0;
            }
            action Abandon when (phase <= 1) {
                custody = 0; revoked = 1; clock = 2;
            }
            action Retire when (phase == 1 && cells == 2) {
                custody = 0; revoked = 1; cells = 1;
            }
            action Replace when (phase == 1 && originals == 1) {
                custody = 0; revoked = 1; originals = 0; births = 3;
            }
            action ObserveUnsampled when (phase == 1) { sample = 0; }
            action ObservePartial when (phase == 1) { sample = 1; }
            action ObserveFull when (phase == 1) { sample = 2; }
            action ObserveMissing when (phase == 1) { sample = 3; }
            action ObserveChanged when (phase == 1) { sample = 4; }
            action TryRestore when (phase == 1) {
                phase = if custody == 1 && originals == 1 && cells == 2
                    && (sample == 2 || Buggy == 1) { 2 } else { 1 };
                clock = if custody == 1 && originals == 1 && cells == 2
                    && (sample == 2 || Buggy == 1) { 1 } else { clock };
                custody = if custody == 1 && originals == 1 && cells == 2
                    && (sample == 2 || Buggy == 1) { 0 } else { custody };
            }

            invariant RestorationNeedsEveryOriginal:
                if phase == 2 { sample == 2 && originals == 1 && revoked == 0 }
                else { originals <= 1 };
            invariant RestorationConservesBirthsAndCells:
                if phase == 2 { cells == 2 && births == 2 } else { cells <= 2 };
            invariant RestorationUsesSavedClock:
                if phase == 2 { clock == 1 && custody == 0 }
                else { if phase == 1 { clock == 2 } else { clock <= 2 } };
            invariant RevokedCustodyCannotReturn:
                if revoked == 1 { custody == 0 && phase <= 1 } else { custody <= 1 };
            invariant StateBounded:
                phase <= 2 && custody <= 1 && revoked <= 1 && originals <= 1
                    && cells > 0 && cells <= 2 && births > 0 && births <= 3
                    && clock > 0 && clock <= 2 && sample <= 4;
        }
    }
}
