// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsAnchor`: formal model of an edit script applied against `line:h6` anchors.

use aterm_spec::derive::Model;

/// Formal model for two-op anchored edit script resolution and atomic commit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn amacs_anchor_model() -> Model {
    aterm_spec::ty_model! {
        AmacsAnchor {
            const Buggy = 0;
            const Lines = 3;
            const Window = 1;
            const MaxMatches = 3;
            const Nth = 2;
            var phase = 0;
            var want = 1;
            var at = 0;
            var near = 0;
            var matches = 0;
            var mode = 0;
            var landed = 0;
            var replaced = 0;
            var staged = 0;
            var applied = 0;
            var outcome = 0;
            action MoveAnchor when (phase == 0 && want <= Lines - 1) {
                want = want + 1;
            }
            action PlaceContent when (phase == 0 && at <= Lines - 1) {
                at = at + 1;
            }
            action GiveNear when (phase == 0 && near == 0) {
                near = 1;
            }
            action AddMatch when (phase == 0 && matches <= MaxMatches - 1) {
                matches = matches + 1;
            }
            action ChooseMode when (phase == 0 && mode <= 1) {
                mode = mode + 1;
            }
            action Start when (phase == 0) {
                phase = 1;
            }
            action LandExact when (phase == 1 && (at == want || (Buggy == 1 && at == want + 1))) {
                landed = want;
                staged = 1;
                phase = 2;
                applied = if Buggy == 2 { applied + 1 } else { applied };
            }
            action Relocate when (
                phase == 1 && near == 1 && at > 0 && (at > want || want > at)
                    && at <= want + Window && want <= at + Window
            ) {
                landed = at;
                staged = 1;
                phase = 2;
                applied = if Buggy == 2 { applied + 1 } else { applied };
            }
            action Conflict when (
                phase == 1 && (at > want || want > at)
                    && (near == 0 || at == 0 || at > want + Window || want > at + Window)
            ) {
                phase = 4;
                outcome = 2;
            }
            action Substitute when (
                phase == 2
                    && ((mode == 0 && matches == 1)
                        || (mode == 1 && matches > 0)
                        || (mode == 2 && matches > Nth - 1)
                        || (Buggy == 3 && mode == 0 && matches > 1))
            ) {
                replaced = if mode == 1 { matches } else { 1 };
                staged = 2;
                phase = 3;
                applied = if Buggy == 2 { applied + 1 } else { applied };
            }
            action SubstituteRefused when (
                phase == 2
                    && ((mode == 0 && (matches == 0 || matches > 1))
                        || (mode == 1 && matches == 0)
                        || (mode == 2 && Nth > matches))
            ) {
                phase = 4;
                outcome = 2;
            }
            action Commit when (phase == 3) {
                applied = staged;
                phase = 4;
                outcome = 1;
            }
            invariant AllOrNothing:
                if outcome == 1 { applied == 2 && staged == 2 } else { applied == 0 };
            invariant AnchorExact:
                landed == 0
                    || (landed == at
                        && (landed == want
                            || (near == 1 && landed <= want + Window && want <= landed + Window)));
            invariant SubstituteUnique:
                replaced == 0
                    || (mode == 0 && matches == 1 && replaced == 1)
                    || (mode == 1 && replaced == matches)
                    || (mode == 2 && replaced == 1 && matches > Nth - 1);
            bound WantInBox: want <= Lines;
            bound ContentInBox: at <= Lines;
            bound MatchesInBox: matches <= MaxMatches;
        }
    }
}
