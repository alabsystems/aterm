// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsAnchor`: an edit script applied against `line:h6` anchors
//! (docs/DESIGN-amacs-2026-10-19.md §7c, §4.9; slice S6).
//!
//! Invariants: AllOrNothing; AnchorExact (an op lands only on a hash match,
//! unless `near=`); SubstituteUnique. Buggy mutants: OffByOne (`Buggy = 1`,
//! in `LandExact`), PartialApply (2, in `LandExact`, `Relocate` and
//! `Substitute`) and FirstMatchWins (3, in `Substitute`). Tier-1: the
//! `#[cfg(test)]` conformance module in `crates/amacs/src/protocol/script.rs`,
//! which drives the real per-op resolver and plan through every scenario of
//! this model's box and writes each mutant as a planner the model must refuse.

use aterm_spec::derive::Model;

/// One plan of a two-op script (amacs's `protocol::script`).
///
/// Op 1 is anchored: its anchor names line `want` (1..`Lines`) with the h6 of
/// some content, and in the pre-text that content sits at line `at` (0: it is
/// gone). The model's hash is injective, so the anchor holds exactly where
/// `at == want`. `near` is 1 when the script was given `near=Window`. Op 2 is
/// a literal substitute whose old text occurs `matches` times, applied in
/// `mode` 0 (it must be unique), 1 (`all`) or 2 (`nth=Nth`). The setup
/// actions (phase 0) choose all of that; `Start` begins the plan.
///
/// The plan resolves the ops in order (phases 1 and 2): `landed` is the line
/// op 1 lands on (0: refused), `replaced` how many matches op 2 replaces, and
/// `staged` how many ops are in the plan. A refusal ends the plan (phase 4,
/// `outcome = 2`); with both ops resolved, `Commit` writes the whole plan
/// (`outcome = 1`). `applied` counts the ops whose edits reached the text.
///
/// Each mutant is its own dial, dead at `Buggy = 0`: an anchor checked one
/// line off, landing on a line whose hash it never checked (`LandExact`,
/// Buggy 1); each op's edits written into the text as it resolves, so a later
/// refusal leaves the first behind (Buggy 2); and a unique substitute that
/// takes the first of several matches (`Substitute`, Buggy 3).
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
