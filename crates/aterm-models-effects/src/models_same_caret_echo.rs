// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::*;

/// A wrapped Codex row may print its first glyph behind an unchanged visible
/// caret while a status repaint replaces the last global print anchor. Its
/// same-caret re-lay requires an exact in-flight key and old hand-owned cell:
/// the row's blank-to-glyph transition is the witness even after the 250 ms
/// key hint, but never after the 10 s press patience. A stalled host can
/// instead observe the first whole a/n/d batch: with no old cell, a prior
/// blank-row probe plus an exact chronological in-flight key run and fresh
/// last key authorize only the missing first cell. A glyph replaced under the
/// unchanged caret is a key's echo only when the hand retyped that column: a
/// Backspace whose retreat no caret move has shown, and the keys typed since
/// it spelling the changed cells (the erase and the retyped keys rendered as
/// one repaint). That change is then the witness, printed or erased to the
/// line's end for a Space, whether the column held the hand's lit cell or
/// the late echo of a key the Backspace erased, which no press paid
/// (`RetypedOverLateEcho`). Tier-1 drives captured Codex frame schedules in
/// `aterm-effects/tests/codex_particle_replay.rs` and Claude Code's composer
/// in `aterm-effects/tests/the_owners_composer_gaps.rs` and
/// `aterm-effects/tests/torn_repaint_hidden_caret.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn same_caret_typed_echo_model() -> Model {
    aterm_spec::ty_model! {
        SameCaretTypedEcho {
            const Buggy = 0;
            var phase = 0;
            var first = 0;
            var second = 0;
            var printed = 0;
            var probed = 0;
            var owned = 0;
            var batch = 0;
            var exact_run = 0;
            var tail_fresh = 0;
            var prior_blank = 0;
            var matches = 0;
            var delayed = 0;
            var live = 0;
            var replaced = 0;
            var retyped = 0;
            var admitted = 0;
            var spent = 0;

            action BankA when (phase == 0) { first = 1; live = 1; phase = 1; }
            action BankN when (phase == 1) { second = 1; }
            action ExactA when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 1; phase = 2;
            }
            action DelayedExactA when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 1;
                delayed = 1; phase = 2;
            }
            action ExpiredExactA when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 1;
                delayed = 1; live = 0; phase = 2;
            }
            action DelayedAmbientOtherGlyph when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 0;
                delayed = 1; phase = 2;
            }
            action AmbientOtherGlyph when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 0; phase = 2;
            }
            action EarlierMatchingPaint when (phase == 1) {
                printed = 0; probed = 1; owned = 1; matches = 1; phase = 2;
            }
            action NoRowProbe when (phase == 1) {
                printed = 1; probed = 0; owned = 1; matches = 1; phase = 2;
            }
            action NoHandOwner when (phase == 1) {
                printed = 1; probed = 1; owned = 0; matches = 1; phase = 2;
            }
            action KeylessPaint when (phase == 0) {
                printed = 1; probed = 1; owned = 1; matches = 1; phase = 2;
            }
            action ExactBatch when (phase == 1) {
                printed = 1; probed = 1; batch = 1; exact_run = 1;
                tail_fresh = 1; prior_blank = 1; matches = 1; phase = 2;
            }
            action BatchOtherGlyph when (phase == 1) {
                printed = 1; probed = 1; batch = 1; exact_run = 0;
                tail_fresh = 1; prior_blank = 1; matches = 0; phase = 2;
            }
            action BatchNoPriorProbe when (phase == 1) {
                printed = 1; probed = 0; batch = 1; exact_run = 1;
                tail_fresh = 1; prior_blank = 0; matches = 1; phase = 2;
            }
            action BatchStaleTail when (phase == 1) {
                printed = 1; probed = 1; batch = 1; exact_run = 1;
                tail_fresh = 0; prior_blank = 1; matches = 1; phase = 2;
            }
            action BatchKeyless when (phase == 0) {
                printed = 1; probed = 1; batch = 1; exact_run = 0;
                tail_fresh = 1; prior_blank = 1; matches = 1; phase = 2;
            }
            action RetypedInPlace when (phase == 1) {
                printed = 0; probed = 1; owned = 1; matches = 1;
                replaced = 1; retyped = 1; phase = 2;
            }
            action RetypedPrinted when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 1;
                replaced = 1; retyped = 1; phase = 2;
            }
            action RetypedOverLateEcho when (phase == 1) {
                printed = 0; probed = 1; owned = 0; matches = 1;
                replaced = 1; retyped = 1; phase = 2;
            }
            action ReplacedNoErase when (phase == 1) {
                printed = 1; probed = 1; owned = 1; matches = 1;
                replaced = 1; retyped = 0; phase = 2;
            }
            action Resolve when (phase == 2) {
                phase = 3;
                admitted = if Buggy == 1 {
                    if delayed == 1 && live == 0 { 1 }
                    else if first == 1 && printed == 1 && probed == 1 && matches == 1
                        && (owned == 1 || (batch == 1 && exact_run == 1
                            && tail_fresh == 1 && prior_blank == 1)) { 0 }
                    else { 1 }
                }
                    else if Buggy == 2 {
                        if first == 1 && printed == 1 && probed == 1
                            && (owned == 1 || batch == 1) { 1 } else { 0 }
                    } else if Buggy == 3 {
                        if first == 1 && matches == 1
                            && (owned == 1 || batch == 1) { 1 } else { 0 }
                    } else if Buggy == 4 {
                        if first == 1 && printed == 1 && probed == 1
                            && matches == 1 && replaced == 0 && (delayed == 0 || live == 1)
                            && (owned == 1 || (batch == 1 && exact_run == 1
                                && tail_fresh == 1 && prior_blank == 1)) { 1 } else { 0 }
                    } else if Buggy == 5 {
                        if first == 1 && printed == 1 && probed == 1
                            && matches == 1 && (delayed == 0 || live == 1)
                            && (owned == 1 || (batch == 1 && exact_run == 1
                                && tail_fresh == 1 && prior_blank == 1)) { 1 } else { 0 }
                    } else if Buggy == 6 {
                        if first == 1 && probed == 1 && matches == 1
                            && (delayed == 0 || live == 1)
                            && ((replaced == 1 && retyped == 1 && owned == 1)
                                || (replaced == 0 && printed == 1
                                    && (owned == 1 || (batch == 1 && exact_run == 1
                                        && tail_fresh == 1 && prior_blank == 1)))) { 1 } else { 0 }
                    } else {
                        if first == 1 && probed == 1 && matches == 1
                            && (delayed == 0 || live == 1)
                            && ((replaced == 1 && retyped == 1)
                                || (replaced == 0 && printed == 1
                                    && (owned == 1 || (batch == 1 && exact_run == 1
                                        && tail_fresh == 1 && prior_blank == 1)))) { 1 } else { 0 }
                    };
                spent = if Buggy == 1 { 0 }
                    else if Buggy == 2 {
                        if first == 1 && printed == 1 && probed == 1
                            && (owned == 1 || batch == 1) { 1 } else { 0 }
                    } else if Buggy == 3 {
                        if first == 1 && matches == 1
                            && (owned == 1 || batch == 1) { 1 } else { 0 }
                    } else if Buggy == 4 {
                        if first == 1 && printed == 1 && probed == 1
                            && matches == 1 && replaced == 0 && (delayed == 0 || live == 1)
                            && (owned == 1 || (batch == 1 && exact_run == 1
                                && tail_fresh == 1 && prior_blank == 1)) { 1 } else { 0 }
                    } else if Buggy == 5 {
                        if first == 1 && printed == 1 && probed == 1
                            && matches == 1 && (delayed == 0 || live == 1)
                            && (owned == 1 || (batch == 1 && exact_run == 1
                                && tail_fresh == 1 && prior_blank == 1)) { 1 } else { 0 }
                    } else if Buggy == 6 {
                        if first == 1 && probed == 1 && matches == 1
                            && (delayed == 0 || live == 1)
                            && ((replaced == 1 && retyped == 1 && owned == 1)
                                || (replaced == 0 && printed == 1
                                    && (owned == 1 || (batch == 1 && exact_run == 1
                                        && tail_fresh == 1 && prior_blank == 1)))) { 1 } else { 0 }
                    } else {
                        if first == 1 && probed == 1 && matches == 1
                            && (delayed == 0 || live == 1)
                            && ((replaced == 1 && retyped == 1)
                                || (replaced == 0 && printed == 1
                                    && (owned == 1 || (batch == 1 && exact_run == 1
                                        && tail_fresh == 1 && prior_blank == 1)))) { 1 } else { 0 }
                    };
            }

            invariant ExactEchoAdmitted:
                if phase == 3 && first == 1 && printed == 1 && probed == 1
                    && owned == 1 && matches == 1 && replaced == 0
                    && (delayed == 0 || live == 1) { admitted == 1 }
                else { admitted <= 1 };
            invariant ExactRetypeAdmitted:
                if phase == 3 && first == 1 && probed == 1
                    && matches == 1 && replaced == 1 && retyped == 1
                    && (delayed == 0 || live == 1) { admitted == 1 }
                else { admitted <= 1 };
            invariant NoReplacedWithoutRetype:
                if replaced == 1 && retyped == 0 { admitted == 0 }
                else { admitted <= 1 };
            invariant ExactBatchAdmitted:
                if phase == 3 && first == 1 && printed == 1 && probed == 1
                    && batch == 1 && exact_run == 1 && tail_fresh == 1
                    && prior_blank == 1 && matches == 1 { admitted == 1 }
                else { admitted <= 1 };
            invariant NoKeylessClaim: admitted <= first;
            invariant NoStalePrintClaim: admitted <= printed + retyped;
            invariant NoStaleProbeClaim: admitted <= probed;
            invariant NoUnownedClaim: admitted <= owned + batch + retyped;
            invariant NoOtherGlyphClaim: admitted <= matches;
            invariant NoExpiredDelayedClaim:
                if delayed == 1 && live == 0 { admitted == 0 }
                else { admitted <= 1 };
            invariant NoUnprovenBatch:
                if batch == 1 { admitted <= exact_run && admitted <= tail_fresh
                    && admitted <= prior_blank } else { admitted <= 1 };
            invariant OneCreditPerAdmission: spent == admitted;
            bound Bounded:
                phase <= 3 && first <= 1 && second <= 1 && printed <= 1
                    && probed <= 1 && owned <= 1 && batch <= 1
                    && exact_run <= 1 && tail_fresh <= 1 && prior_blank <= 1
                    && matches <= 1 && delayed <= 1 && live <= 1
                    && replaced <= 1 && retyped <= 1
                    && admitted <= 1 && spent <= 1;
        }
    }
}

/// A Backspace's echo, in byte order. Bytes are processed in order: a key
/// typed and erased inside one echo lag (`thes⌫`, `s` unseen at the
/// Backspace) still prints before the erase's retreat, and a key typed after
/// the Backspace echoes after it. So the erased key's late echo is paid by no
/// press (the Backspace retired it), the next key's press included, and it
/// leaves the erase owed: the retreat, however late, is the erase's echo,
/// never Ink's park while the next key is in flight. A correction rendered as
/// one repaint (`MergedCorrection`) is the retreat and the next key's advance
/// at once. Before the law the late echo spent the Backspace's licence
/// (`Buggy = 2`): the retreat was held as a park, the next key's echo back to
/// the park's origin cancelled it, and that key stayed dark (the owner's
/// `the▯subtrees`, 2026-10-16). Paying the late echo with the next key's
/// press (`Buggy = 1`) darkens that key too. Tier-1 drives Claude Code's
/// composer at every echo lag in
/// `aterm-effects/tests/torn_repaint_hidden_caret.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn erased_key_echo_order_model() -> Model {
    aterm_spec::ty_model! {
        ErasedKeyEchoOrder {
            const Buggy = 0;
            var phase = 0;
            var retired = 0;
            var next = 0;
            var owed = 0;
            var late_paid = 0;
            var parked = 0;
            var retreated = 0;
            var lit = 0;

            action EraseEchoed when (phase == 0) { owed = 1; phase = 1; }
            action EraseUnechoed when (phase == 0) { owed = 1; retired = 1; phase = 1; }
            action NextKey when (phase > 0 && phase <= 3 && next == 0) { next = 1; }
            action LateEcho when (phase == 1 && retired == 1) {
                late_paid = if Buggy == 1 { next } else { 0 };
                owed = if Buggy == 2 { 0 } else { owed };
                phase = 2;
            }
            action Retreat when ((phase == 1 && retired == 0) || phase == 2) {
                parked = if owed == 0 && next == 1 { 1 } else { 0 };
                retreated = owed;
                owed = 0;
                phase = 3;
            }
            action MergedCorrection
                when (((phase == 1 && retired == 0) || phase == 2) && next == 1)
            {
                retreated = owed;
                lit = if owed == 1 && late_paid == 0 { 1 } else { 0 };
                owed = 0;
                phase = 4;
            }
            action NextEcho when (phase == 3 && next == 1) {
                lit = if parked == 0 && late_paid == 0 { 1 } else { 0 };
                phase = 4;
            }

            invariant NoPaidLateEcho: late_paid == 0;
            invariant OwedRetreatNotParked: parked == 0;
            invariant RetreatIsTheErase:
                if phase > 2 { retreated == 1 } else { retreated <= 1 };
            invariant NextKeyLit: if phase == 4 { lit == 1 } else { lit <= 1 };
            bound Bounded:
                phase <= 4 && retired <= 1 && next <= 1 && owed <= 1
                    && late_paid <= 1 && parked <= 1 && retreated <= 1 && lit <= 1;
        }
    }
}

/// An unknown-width insert can absorb a queued key into its own hop, so its
/// dispatch-to-delivery credits leave the generic ring at orphan cleanup.
/// When one or two known one-cell keys remain, the seam escrows their glyphs
/// and the insert's row/next column/print generation. Exact later row
/// transitions spend one key or both when a single frame has two new exact
/// cells; a different print, a third pending key, expiry, scroll, rewrite, or
/// reset closes the remainder. The PTY cannot distinguish a delayed key from
/// program output that exactly mimics its glyphs and transition; the model
/// rejects every distinguishable program move. Tier-1 drives the genuine
/// CursorGlow seam in `cursor_glow::tests::unknown_insert`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn unknown_insert_orphan_key_model() -> Model {
    aterm_spec::ty_model! {
        UnknownInsertOrphanKey {
            const Buggy = 0;
            var phase = 0;
            var queued = 0;
            var generic = 0;
            var site = 0;
            var escrow = 0;
            var exact = 0;
            var lit = 0;

            action QueueOne when (phase == 0) {
                queued = 1; generic = 1; phase = 1;
            }
            action QueueBlank when (phase == 0) {
                queued = 4; generic = 1; phase = 1;
            }
            action QueueTwo when (phase == 0) {
                queued = 2; generic = 2; phase = 1;
            }
            action QueueThree when (phase == 0) {
                queued = 3; generic = 3; phase = 1;
            }
            action QueueNone when (phase == 0) { phase = 1; }
            action LayWithSite when (phase == 1) { site = 1; phase = 2; }
            action LayWithoutSite when (phase == 1) { phase = 2; }
            action Cleanup when (phase == 2) {
                generic = if Buggy == 1 { generic } else { 0 };
                escrow = if (queued == 1 || queued == 2) && site == 1
                    { queued } else { 0 };
                phase = 3;
            }
            action ExactNewPrint when (phase == 3) {
                exact = 1;
                lit = if Buggy == 1 || (escrow > 0 && site == 1) { 1 } else { 0 };
                escrow = if Buggy == 1 { escrow }
                    else if escrow > 0 { escrow - 1 } else { 0 };
                phase = 4;
            }
            action CoalescedTwoExactNewPrint when (phase == 3 && queued == 2) {
                exact = 2;
                lit = if Buggy == 1 || (escrow == 2 && site == 1) { 2 } else { 0 };
                escrow = 0; phase = 5;
            }
            action CoalescedTwoWrongOrPartial when (phase == 3 && queued == 2) {
                escrow = 0; phase = 5;
            }
            action CoalescedTwoOldPrint when (phase == 3 && queued == 2) {
                escrow = 0; phase = 5;
            }
            action CoalescedTwoMissingProbe when (phase == 3 && queued == 2) {
                escrow = 0; phase = 5;
            }
            action CoalescedTwoAltWithoutBlink when (phase == 3 && queued == 2) {
                escrow = 0; phase = 5;
            }
            action CoalescedTooMany when (phase == 3 && queued == 3) {
                escrow = 0; phase = 5;
            }
            action AmbientOtherGlyph when (phase == 3) {
                lit = if Buggy == 1 { 1 } else { 0 };
                escrow = 0; phase = 4;
            }
            action ExactOldPrint when (phase == 3) {
                escrow = 0; phase = 4;
            }
            action Expire when (phase == 3) {
                escrow = 0; phase = 4;
            }
            action Scroll when (phase == 3) {
                site = 0; escrow = 0; phase = 4;
            }
            action Rewrite when (phase == 3) {
                site = 0; escrow = 0; phase = 4;
            }
            action Reset when (phase == 3) {
                site = 0; escrow = 0; phase = 4;
            }
            action SecondExactNewPrint when (phase == 4 && queued == 2 && escrow == 1) {
                exact = 2; lit = 2; escrow = 0; phase = 5;
            }
            action SecondAmbientOtherGlyph when (phase == 4 && queued == 2 && escrow == 1) {
                escrow = 0; phase = 5;
            }
            action SecondExactOldPrint when (phase == 4 && queued == 2 && escrow == 1) {
                escrow = 0; phase = 5;
            }
            action SecondPrefixRewrite when (phase == 4 && queued == 2 && escrow == 1) {
                escrow = 0; phase = 5;
            }
            action LaterProgramPrint when (phase == 4) { escrow = 0; phase = 5; }

            invariant NoGenericAfterCleanup:
                if phase > 2 { generic == 0 } else { generic <= 3 };
            invariant ExactOnly: lit <= exact;
            invariant SiteRequired:
                if site == 0 { lit == 0 } else { lit <= 2 };
            invariant OneShot: lit + escrow <= queued;
            bound Bounded:
                phase <= 5 && queued <= 4 && generic <= 3 && site <= 1
                    && escrow <= 2 && exact <= 2 && lit <= 2;
        }
    }
}
