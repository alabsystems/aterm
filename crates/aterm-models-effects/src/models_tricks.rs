// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kitty commands (`aterm-effects`): the typed-line listener that decides a word was said to the pet (`typed_tricks.rs`), and the flash that lights that word on the glass (`trick_flash.rs`).

use aterm_spec::derive::*;

/// THE LISTENER's purity, poison and prefix laws (`SPECS.md`'s `tt-fire-licence-safety`, `tt-count-belief`, `tt-poison-and-reset` and `tt-session-dirty-memory`, whose gap 10 this model and its bind are): when a typed line may speak to the pet, and what takes that right away.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trick_listener_purity_model() -> Model {
    aterm_spec::ty_model! {
        TrickListenerPurity {
            const Buggy = 0;
            var pure = 1;
            var poisoned = 0;
            var tentative = 0;
            var named = 0;
            var count = 0;
            var real = 0;
            var open = 0;
            var hushed = 0;
            var snap = 0;
            var snap_named = 0;
            var ev = 0;
            var said = 0;
            var restored = 0;
            var was_pure = 1;
            var was_poisoned = 0;
            var was_named = 0;

            action Letter when (poisoned == 0) {
                hushed = 0;
                open = 1; count = 1; real = 1;
                said = if Buggy == 1 && pure == 1 && open == 1 { 1 } else { 0 };
                tentative = if Buggy == 1 && pure == 1 && open == 1 { 1 } else { tentative };
                ev = 1; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action LetterGlued when (poisoned == 0 && pure == 1 && open == 0 && count == 1) {
                hushed = 0;
                pure = 0; tentative = 0;
                said = if tentative == 1 { 4 } else { 0 };
                open = 1; real = 1;
                ev = 1; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action MarkQuiet when (poisoned == 0) {
                snap = if open == 1
                    { if pure == 1 { if tentative == 1 { 2 } else { 1 } } else { 3 } }
                    else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                open = 0; count = 1; real = 1;
                said = if Buggy == 1 && pure == 1 && open == 1 { 1 } else { 0 };
                tentative = if Buggy == 1 && pure == 1 && open == 1 { 1 } else { tentative };
                ev = 2; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action MarkName when (poisoned == 0) {
                snap = if open == 1
                    { if pure == 1 { if tentative == 1 { 2 } else { 1 } } else { 3 } }
                    else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                named = if pure == 1 { 1 } else { named };
                open = 0; count = 1; real = 1; said = 0;
                ev = 12; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action MarkImpure when (poisoned == 0 && pure == 1) {
                snap = if open == 1 { if tentative == 1 { 2 } else { 1 } } else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                pure = 0; tentative = 0;
                said = if tentative == 1 { 4 } else { 0 };
                open = 0; count = 1; real = 1;
                ev = 2; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action EdgeQuiet when (poisoned == 0) {
                snap = if open == 1
                    { if pure == 1 { if tentative == 1 { 2 } else { 1 } } else { 3 } }
                    else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                open = 0; real = 1;
                count = if Buggy == 1 { count } else { 1 };
                named = if Buggy == 1 && open == 1 { 1 } else { named };
                said = if Buggy == 1 && pure == 0 && open == 1 { 1 } else { 0 };
                tentative = if Buggy == 1 && pure == 0 && open == 1 { 1 } else { tentative };
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EdgeFire when (poisoned == 0 && pure == 1 && (hushed == 0 || Buggy == 1)) {
                snap = if open == 1 { if tentative == 1 { 2 } else { 1 } } else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                open = 0; count = 1; real = 1;
                said = if named == 1 || Buggy == 1 { 2 } else { 1 };
                tentative = if named == 1 || Buggy == 1 { 0 } else { 1 };
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EdgeConfirm
                when (poisoned == 0 && pure == 1 && open == 0 && tentative == 1 && named == 1
                    && hushed == 0) {
                said = 3; tentative = 0; count = 1; real = 1;
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EdgeUnknown when (poisoned == 0 && pure == 1 && open == 1) {
                snap = if tentative == 1 { 2 } else { 1 };
                snap_named = named;
                pure = 0;
                tentative = if Buggy == 1 { tentative } else { 0 };
                said = if Buggy == 1 { 0 } else { if tentative == 1 { 4 } else { 0 } };
                open = 0; count = 1; real = 1;
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action EdgeGlued when (poisoned == 0) {
                hushed = 1;
                snap = if open == 1
                    { if pure == 1 { if tentative == 1 { 2 } else { 1 } } else { 3 } }
                    else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                open = 0; count = 1; real = 1;
                named = if Buggy == 1 && pure == 1 { 1 } else { named };
                said = if Buggy == 1 && pure == 1 { 2 } else { 0 };
                tentative = if Buggy == 1 && pure == 1 { 0 } else { tentative };
                ev = 13; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EdgeGluedUnknown when (poisoned == 0 && pure == 1 && open == 1) {
                hushed = 0;
                snap = if tentative == 1 { 2 } else { 1 };
                snap_named = named;
                pure = 0; tentative = 0;
                said = if tentative == 1 { 4 } else { 0 };
                open = 0; count = 1; real = 1;
                ev = 13; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action EdgeName when (poisoned == 0) {
                hushed = 0;
                snap = if open == 1
                    { if pure == 1 { if tentative == 1 { 2 } else { 1 } } else { 3 } }
                    else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                named = if pure == 1 { 1 } else { named };
                said = if pure == 1 && tentative == 1 { 3 } else { 0 };
                tentative = 0;
                open = 0; count = 1; real = 1;
                ev = 4; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EdgeNameFires when (poisoned == 0 && pure == 1 && tentative == 0) {
                hushed = 0;
                snap = if open == 1 { 1 } else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                named = 1; said = 2;
                open = 0; count = 1; real = 1;
                ev = 4; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action SubmitQuiet when (tentative == 0) {
                hushed = 0;
                said = if Buggy == 1 && poisoned == 1 { 2 } else { 0 };
                pure = 1; poisoned = 0; named = 0; count = 0; real = 0; open = 0;
                snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action SubmitFinal when (poisoned == 0 && pure == 1) {
                hushed = 0;
                said = 2;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action SubmitConfirm when (poisoned == 0 && pure == 1 && tentative == 1) {
                hushed = 0;
                said = 3;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action SubmitRevoke when (poisoned == 0 && pure == 1 && tentative == 1 && open == 1) {
                hushed = 0;
                said = 4;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action EraseSome when (poisoned == 0 && count == 1) {
                pure = if Buggy == 1 { 1 } else { pure };
                said = 0;
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EraseToken when (poisoned == 0 && open == 1 && count == 1) {
                open = 0; said = 0;
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EraseBlind when (poisoned == 0 && open == 0 && count == 1) {
                pure = 0; tentative = 0;
                said = if tentative == 1 { 4 } else { 0 };
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EraseLast when (poisoned == 0 && count == 1) {
                hushed = 0;
                said = if tentative == 1 { 4 } else { 0 };
                pure = 1; tentative = 0; named = 0; count = 0; real = 0; open = 0;
                snap = 0; snap_named = 0;
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action Restore when (poisoned == 0 && open == 0 && count == 1 && snap > 0) {
                hushed = 0;
                pure = if snap <= 2 || Buggy == 1 { 1 } else { 0 };
                tentative = if snap == 2 { 1 } else { 0 };
                named = snap_named; open = 1; snap = 0; said = 0;
                ev = 6; restored = snap; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action RestoreRevoke
                when (poisoned == 0 && open == 0 && count == 1 && snap > 0 && tentative == 1) {
                hushed = 0;
                pure = if snap <= 2 { 1 } else { 0 };
                tentative = if snap == 2 { 1 } else { 0 };
                named = snap_named; open = 1; snap = 0; said = 4;
                ev = 6; restored = snap; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action Reset {
                hushed = 0;
                said = if Buggy == 1 && pure == 1 && poisoned == 0 && open == 1 { 2 }
                       else { if tentative == 1 { 4 } else { 0 } };
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 7; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action Lose {
                hushed = 0;
                poisoned = 1;
                tentative = if Buggy == 1 { tentative } else { 0 };
                said = if Buggy == 1 { 0 } else { if tentative == 1 { 4 } else { 0 } };
                open = 0; snap = 0;
                ev = 8; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action RekeyClean {
                hushed = 0;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0; said = 0;
                ev = 9; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action RekeyDirty {
                hushed = 0;
                poisoned = if Buggy == 1 { 0 } else { 1 };
                pure = 1; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0; said = 0;
                ev = 10; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            action Ignored when (poisoned == 1 || count == 0) {
                poisoned = if Buggy == 1 { 0 } else { poisoned };
                said = 0;
                ev = 11; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            invariant OnlyAPureLineSpeaks: said == 0 || said == 4 || was_pure == 1;
            invariant APoisonedLineNeverSpeaks: said == 0 || said == 4 || was_poisoned == 0;
            invariant APrefixNeverSpeaks: ev > 2 || said == 0 || said == 4;
            invariant EditingKeysOnlyRevoke: ev <= 5 || said == 0 || said == 4;
            invariant AFinalFireNeedsANamedLine: said <= 1 || said > 3 || ev == 5 || named == 1;
            invariant OnlyTheNameNamesTheLine:
                ev == 4 || ev == 12 || named == 0 || was_named == 1;
            invariant AGluedRunWaitsForEnterOrTheName: hushed == 0 || said == 0 || said == 4;
            invariant PoisonHoldsNoTentative: poisoned == 0 || tentative == 0;
            invariant ImpurityHoldsNoTentative: pure == 1 || tentative == 0;
            invariant ZeroCountIsAnEmptyLine: poisoned == 1 || count == 1 || real == 0;
            invariant ImpurityLiftsOnlyLawfully:
                was_pure == 1 || pure == 0 || count == 0 || (restored > 0 && restored <= 2);
            invariant PoisonLiftsOnlyByAReset:
                was_poisoned == 0 || poisoned == 1 || ev == 5 || ev == 7 || ev == 9;
            invariant ADirtySessionReturnsPoisoned: ev <= 9 || ev > 10 || poisoned == 1;
        }
    }
}

/// THE FLASH's pane-scope and revoke laws (`SPECS.md`'s `flash-lifecycle` and `flash-pane-scope`): one window-wide slot that names the pane its key went to, or none.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trick_flash_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        TrickFlashLifecycle {
            const Buggy = 0;
            var phase = 0;
            var named = 0;
            var ev = 0;
            var was_phase = 0;

            action NotePane { phase = 1; named = 1; ev = 1; was_phase = phase; }
            action NoteWindow { phase = 1; named = 0; ev = 1; was_phase = phase; }
            action Find {
                phase = if phase == 1 || (Buggy == 1 && phase == 0) { 2 } else { phase };
                ev = 2; was_phase = phase;
            }
            action Revoke {
                phase = if phase == 1 {
                    if Buggy == 1 { 1 } else { 0 }
                } else if phase == 2 {
                    if Buggy == 1 && named == 0 { 0 } else { 3 }
                } else if phase == 3 && Buggy == 1 {
                    0
                } else {
                    phase
                };
                ev = 3; was_phase = phase;
            }
            action CloseOwn when (named == 1) {
                phase = if Buggy == 1 { phase } else { 0 };
                ev = 4; was_phase = phase;
            }
            action CloseOther {
                phase = if Buggy == 1 { 0 } else { phase };
                ev = 5; was_phase = phase;
            }
            action Rebind {
                phase = if named == 1 && phase > 1 {
                    if Buggy == 1 { 0 } else { phase }
                } else if Buggy == 1 {
                    phase
                } else {
                    0
                };
                ev = 6; was_phase = phase;
            }

            invariant LiveOnlyByFinding:
                phase <= 1 || phase > 2 || was_phase == 2 || (was_phase == 1 && ev == 2);
            invariant ARevokeDropsAnUnfoundWord:
                ev <= 2 || ev > 3 || was_phase == 0 || was_phase > 1 || phase == 0;
            invariant ARevokeFadesALiveFlash:
                ev <= 2 || ev > 3 || was_phase <= 1 || was_phase > 2 || phase == 3;
            invariant ASecondRevokeKeepsTheFade: ev <= 2 || ev > 3 || was_phase <= 2 || phase == 3;
            invariant ClosingItsPaneEndsTheFlash: ev <= 3 || ev > 4 || phase == 0;
            invariant ClosingAnotherPaneLeavesTheFlash: ev <= 4 || ev > 5 || phase == was_phase;
            invariant ABindFromUnboundKeepsAPanesFoundFlash:
                ev <= 5 || named == 0 || was_phase <= 1 || phase == was_phase;
            invariant ABindFromUnboundRetiresEveryOtherFlash:
                ev <= 5 || (named == 1 && was_phase > 1) || phase == 0;
        }
    }
}
