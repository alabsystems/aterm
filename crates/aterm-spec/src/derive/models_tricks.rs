// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kitty commands (`aterm-effects`): the typed-line listener that decides a
//! word was said to the pet (`typed_tricks.rs`), and the flash that lights that
//! word on the glass (`trick_flash.rs`).
//!
//! Both are specification automata, not copies: the words, the phrase law, the
//! locator and the clocks are the code's to work out. What a law here says is
//! what every answer the code gives must respect. The laws `aterm-effects/SPECS.md`
//! already routes to Trust about one function (`tt-firing-tiers`, `tt-phrase-law`,
//! `tt-punctuation-park`, `tt-backspace-undo`, `flash-locate-row-rule`,
//! `flash-note-admission`) are cited there and left out here.

use super::*;

/// THE LISTENER's purity, poison and prefix laws (`SPECS.md`'s
/// `tt-fire-licence-safety`, `tt-count-belief`, `tt-poison-and-reset` and
/// `tt-session-dirty-memory`, whose gap 10 this model and its bind are): when
/// a typed line may speak to the pet, and what takes that right away.
///
/// What a boundary says is nondeterministic (`EdgeQuiet` / `EdgeFire` /
/// `EdgeUnknown`, the four `Submit*`); the listener resolves it. The model
/// admits steps the listener never takes, never the reverse.
///
///   * `pure`, `poisoned` are the listener's observers; `count` is
///     `line_chars()` folded to zero / some and `real` the same fold of the
///     line an editor holds. `tentative`, `named` (the line has named the pet),
///     `open` (a token stands at the caret) and `snap` (the one boundary
///     Backspace can cross back: 0 none, 1 taken pure, 2 pure with a tentative
///     fire live, 3 impure; `snap_named` beside it) are hidden.
///   * `ev`, the input class, is the trace's, never inferred from the answer:
///     1 a word character, 2 a mark (`. ! ? ,`, code punctuation), 3 an open
///     edge (whitespace, a fullwidth stop) closing anything but the pet's name
///     — a glued `猫咪坐下` among them, 4 one closing the name, 5 Enter,
///     6 Backspace or a word-kill every editor agrees about, 7 a line reset,
///     8 a press that loses the line (a chord, a paste, a no-echo prompt),
///     9 / 10 a switch to a session left clean / dirty, 11 a key the line
///     ignores (anything under poison, an edit of an empty line), 12 a stop
///     (`. ! ? ,`) closing the name: it names the line and parks what the name
///     says until the next open edge (`good kitty! `, `sit kitty. `).
///   * `said`: 0 nothing, 1 a tentative Fire, 2 a final Fire, 3 Confirm,
///     4 Revoke. `was_*` and `restored` (the snapshot this press consumed)
///     read the press's pre-state.
///
/// `Buggy = 1` is one defect per law: `Letter` and `MarkQuiet` fire on the
/// completing letter (`sit` of `site`); `EdgeQuiet` fires without asking for
/// purity (`npm run sit`), names the line at a token that is no name (the
/// name inside `猫咪坐下`) and does not count its delimiter; `EdgeFire` makes a
/// bare word final (`sleep 5`); `SubmitQuiet` judges a poisoned line; `Lose`
/// and `EdgeUnknown` forget the revoke; `EraseSome` calls a line clean once the
/// offending character is gone and `Restore` calls any snapshot pure
/// (`npm sit␣⌫␣`); `Ignored` lets a key recover a poisoned line; `Reset`
/// judges the token it abandons; `RekeyDirty` comes back clean.
///
/// Tier-0: `aterm-spec/tests/derived_trick_listener_purity.rs`. Tier-1:
/// `aterm-effects/tests/conformance_trick_listener.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trick_listener_purity_model() -> Model {
    crate::ty_model! {
        TrickListenerPurity {
            const Buggy = 0;
            var pure = 1;
            var poisoned = 0;
            var tentative = 0;
            var named = 0;
            var count = 0;
            var real = 0;
            var open = 0;
            var snap = 0;
            var snap_named = 0;
            var ev = 0;
            var said = 0;
            var restored = 0;
            var was_pure = 1;
            var was_poisoned = 0;
            var was_named = 0;

            // ---- 1: a word character ----
            action Letter when (poisoned == 0) {
                open = 1; count = 1; real = 1;
                said = if Buggy == 1 && pure == 1 && open == 1 { 1 } else { 0 };
                tentative = if Buggy == 1 && pure == 1 && open == 1 { 1 } else { tentative };
                ev = 1; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // The glue law: a word character right after a stop is `sit.txt`.
            action LetterGlued when (poisoned == 0 && pure == 1 && open == 0 && count == 1) {
                pure = 0; tentative = 0;
                said = if tentative == 1 { 4 } else { 0 };
                open = 1; real = 1;
                ev = 1; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 2 / 12: a mark closes a token; it never speaks for one ----
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

            // ---- 3: an open edge closing anything but the name ----
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
            // Final on a named line, tentative on any other. A glued run's
            // name is inside its own token and names nothing; the run says
            // nothing at its own edge (`EdgeQuiet`).
            action EdgeFire when (poisoned == 0 && pure == 1) {
                snap = if open == 1 { if tentative == 1 { 2 } else { 1 } } else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                open = 0; count = 1; real = 1;
                said = if named == 1 || Buggy == 1 { 2 } else { 1 };
                tentative = if named == 1 || Buggy == 1 { 0 } else { 1 };
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // What a stop closing the name parked: its confirmation.
            action EdgeConfirm
                when (poisoned == 0 && pure == 1 && open == 0 && tentative == 1 && named == 1) {
                said = 3; tentative = 0; count = 1; real = 1;
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // A token that is no pet word: the line is prose from here on.
            action EdgeUnknown when (poisoned == 0 && pure == 1 && open == 1) {
                snap = if tentative == 1 { 2 } else { 1 };
                snap_named = named;
                pure = 0;
                tentative = if Buggy == 1 { tentative } else { 0 };
                said = if Buggy == 1 { 0 } else { if tentative == 1 { 4 } else { 0 } };
                open = 0; count = 1; real = 1;
                ev = 3; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 4: the pet's name: it confirms a tentative fire, or fires a
            // held address word ----
            action EdgeName when (poisoned == 0) {
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
                snap = if open == 1 { 1 } else { snap };
                snap_named = if open == 1 { named } else { snap_named };
                named = 1; said = 2;
                open = 0; count = 1; real = 1;
                ev = 4; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 5: Enter. A live tentative fire is always answered, and the
            // next line is fresh ----
            action SubmitQuiet when (tentative == 0) {
                said = if Buggy == 1 && poisoned == 1 { 2 } else { 0 };
                pure = 1; poisoned = 0; named = 0; count = 0; real = 0; open = 0;
                snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action SubmitFinal when (poisoned == 0 && pure == 1) {
                said = 2;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action SubmitConfirm when (poisoned == 0 && pure == 1 && tentative == 1) {
                said = 3;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action SubmitRevoke when (poisoned == 0 && pure == 1 && tentative == 1 && open == 1) {
                said = 4;
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 5; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 6: Backspace, or a measured word-kill ----
            // A character goes and text is left: nothing the laws speak of moves.
            action EraseSome when (poisoned == 0 && count == 1) {
                pure = if Buggy == 1 { 1 } else { pure };
                said = 0;
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action EraseToken when (poisoned == 0 && open == 1 && count == 1) {
                open = 0; said = 0;
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // A second boundary crossed backwards: what stands left of the caret
            // was never kept.
            action EraseBlind when (poisoned == 0 && open == 0 && count == 1) {
                pure = 0; tentative = 0;
                said = if tentative == 1 { 4 } else { 0 };
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // The count reaches zero: a provably empty line.
            action EraseLast when (poisoned == 0 && count == 1) {
                said = if tentative == 1 { 4 } else { 0 };
                pure = 1; tentative = 0; named = 0; count = 0; real = 0; open = 0;
                snap = 0; snap_named = 0;
                ev = 6; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // Back over the character that closed the last token: the line is
            // the snapshot again, impure if that is what it was.
            action Restore when (poisoned == 0 && open == 0 && count == 1 && snap > 0) {
                pure = if snap <= 2 || Buggy == 1 { 1 } else { 0 };
                tentative = if snap == 2 { 1 } else { 0 };
                named = snap_named; open = 1; snap = 0; said = 0;
                ev = 6; restored = snap; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            // The same, un-typing the boundary a tentative fire was made at.
            action RestoreRevoke
                when (poisoned == 0 && open == 0 && count == 1 && snap > 0 && tentative == 1) {
                pure = if snap <= 2 { 1 } else { 0 };
                tentative = if snap == 2 { 1 } else { 0 };
                named = snap_named; open = 1; snap = 0; said = 4;
                ev = 6; restored = snap; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 7: the line was abandoned or emptied without being judged ----
            action Reset {
                said = if Buggy == 1 && pure == 1 && poisoned == 0 && open == 1 { 2 }
                       else { if tentative == 1 { 4 } else { 0 } };
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0;
                ev = 7; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 8: the line now holds something the listener did not see.
            // The count stops where it was, so zero proves nothing from here. ----
            action Lose {
                poisoned = 1;
                tentative = if Buggy == 1 { tentative } else { 0 };
                said = if Buggy == 1 { 0 } else { if tentative == 1 { 4 } else { 0 } };
                open = 0; snap = 0;
                ev = 8; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 9 / 10: the window's front session changed ----
            action RekeyClean {
                pure = 1; poisoned = 0; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0; said = 0;
                ev = 9; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }
            action RekeyDirty {
                poisoned = if Buggy == 1 { 0 } else { 1 };
                pure = 1; tentative = 0; named = 0; count = 0; real = 0;
                open = 0; snap = 0; snap_named = 0; said = 0;
                ev = 10; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // ---- 11: a key the line does not take ----
            action Ignored when (poisoned == 1 || count == 0) {
                poisoned = if Buggy == 1 { 0 } else { poisoned };
                said = 0;
                ev = 11; restored = 0; was_pure = pure; was_poisoned = poisoned; was_named = named;
            }

            // A Fire or a Confirm comes only from a pure, unpoisoned line, and
            // only at an open edge or Enter: never from a prefix (`site`) or a
            // mark (`sit.txt`), never from an editing key.
            invariant OnlyAPureLineSpeaks: said == 0 || said == 4 || was_pure == 1;
            invariant APoisonedLineNeverSpeaks: said == 0 || said == 4 || was_poisoned == 0;
            invariant APrefixNeverSpeaks: ev > 2 || said == 0 || said == 4;
            invariant EditingKeysOnlyRevoke: ev <= 5 || said == 0 || said == 4;
            // A word is final, or confirmed, only on a line that named the pet
            // (or at Enter), and only the name names a line: `猫咪坐下` alone
            // never is.
            invariant AFinalFireNeedsANamedLine: said <= 1 || said > 3 || ev == 5 || named == 1;
            invariant OnlyTheNameNamesTheLine:
                ev == 4 || ev == 12 || named == 0 || was_named == 1;
            invariant PoisonHoldsNoTentative: poisoned == 0 || tentative == 0;
            invariant ImpurityHoldsNoTentative: pure == 1 || tentative == 0;
            // `line_chars() == 0` proves the line empty only while unpoisoned.
            invariant ZeroCountIsAnEmptyLine: poisoned == 1 || count == 1 || real == 0;
            // Impurity lifts only on an empty line or by restoring a snapshot
            // taken while pure.
            invariant ImpurityLiftsOnlyLawfully:
                was_pure == 1 || pure == 0 || count == 0 || (restored > 0 && restored <= 2);
            // Poison lifts only by Enter, a line reset, or a clean session.
            invariant PoisonLiftsOnlyByAReset:
                was_poisoned == 0 || poisoned == 1 || ev == 5 || ev == 7 || ev == 9;
            invariant ADirtySessionReturnsPoisoned: ev <= 9 || ev > 10 || poisoned == 1;
        }
    }
}

/// THE FLASH's pane-scope and revoke laws (`SPECS.md`'s `flash-lifecycle` and
/// `flash-pane-scope`): one window-wide slot that names the pane its key went
/// to, or none.
///
///   * `phase` is `trick_flash_phase(now)` inside the flash's window: 0 idle,
///     1 pending (noted, not found), 2 live, 3 revoked and fading. `named` (the
///     slot names a pane) is hidden.
///   * `ev`: 1 a note, 2 a rescan of the flash's pane that finds the word,
///     3 a revoke, 4 / 5 the layout dropped the flash's pane / another one,
///     6 a bind from unbound (the engine's pane-space reset, after the bound
///     pane closed or the window first split). A bind between panes, a rescan
///     that finds nothing and another pane's rescan are no step.
///
/// `Buggy = 1` is one defect per law: a rescan that finds a word nobody noted;
/// a revoke that keeps an unfound word, cuts the window's live flash, or cuts a
/// pane's fade on its second call; a flash that outlives its pane, dies with a
/// sibling, or dies with the reset a sibling's close leaves behind (the bug the
/// bind's `found_in_a_pane` fixed) while the window-wide grid's own flash, and
/// an unfound word whose caret may be that grid's, survive it.
///
/// Where a word is found is the locator's (`flash-locate-row-rule`): a late
/// echo that must not start a search is no step here, and is held by
/// `trick_flash.rs`'s own tests.
///
/// Tier-0: `aterm-spec/tests/derived_trick_flash_lifecycle.rs`. Tier-1:
/// `aterm-effects/tests/conformance_trick_flash.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trick_flash_lifecycle_model() -> Model {
    crate::ty_model! {
        TrickFlashLifecycle {
            const Buggy = 0;
            var phase = 0;
            var named = 0;
            var ev = 0;
            var was_phase = 0;

            // One slot: the newest note replaces whatever stood there.
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

            // Never paint a place not found: live only from pending, by finding.
            invariant LiveOnlyByFinding:
                phase <= 1 || phase > 2 || was_phase == 2 || (was_phase == 1 && ev == 2);
            // The revoke: an unfound word goes at once, a live one fades, and a
            // second call leaves the fade alone.
            invariant ARevokeDropsAnUnfoundWord:
                ev <= 2 || ev > 3 || was_phase == 0 || was_phase > 1 || phase == 0;
            invariant ARevokeFadesALiveFlash:
                ev <= 2 || ev > 3 || was_phase <= 1 || was_phase > 2 || phase == 3;
            invariant ASecondRevokeKeepsTheFade: ev <= 2 || ev > 3 || was_phase <= 2 || phase == 3;
            // One slot, and it names its pane.
            invariant ClosingItsPaneEndsTheFlash: ev <= 3 || ev > 4 || phase == 0;
            invariant ClosingAnotherPaneLeavesTheFlash: ev <= 4 || ev > 5 || phase == was_phase;
            invariant ABindFromUnboundKeepsAPanesFoundFlash:
                ev <= 5 || named == 0 || was_phase <= 1 || phase == was_phase;
            invariant ABindFromUnboundRetiresEveryOtherFlash:
                ev <= 5 || (named == 1 && was_phase > 1) || phase == 0;
        }
    }
}
