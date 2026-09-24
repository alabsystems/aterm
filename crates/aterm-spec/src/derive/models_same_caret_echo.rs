// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A wrapped Codex row may print its first glyph behind an unchanged visible
/// caret while a status repaint replaces the last global print anchor. Its
/// same-caret re-lay requires an exact in-flight key and old hand-owned cell:
/// the row's blank-to-glyph transition is the witness even after the 250 ms
/// key hint, but never after the 10 s press patience. A stalled host can
/// instead observe the first whole a/n/d batch: with no old cell, a prior
/// blank-row probe plus an exact chronological in-flight key run and fresh
/// last key authorize only the missing first cell. Tier-1 drives captured
/// Codex frame schedules in
/// `aterm-effects/tests/codex_particle_replay.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn same_caret_typed_echo_model() -> Model {
    crate::ty_model! {
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
                    } else {
                        if first == 1 && printed == 1 && probed == 1
                            && matches == 1 && (delayed == 0 || live == 1)
                            && (owned == 1 || (batch == 1 && exact_run == 1
                                && tail_fresh == 1 && prior_blank == 1)) { 1 } else { 0 }
                    };
                spent = if Buggy == 1 { 0 }
                    else if Buggy == 2 {
                        if first == 1 && printed == 1 && probed == 1
                            && (owned == 1 || batch == 1) { 1 } else { 0 }
                    } else if Buggy == 3 {
                        if first == 1 && matches == 1
                            && (owned == 1 || batch == 1) { 1 } else { 0 }
                    } else {
                        if first == 1 && printed == 1 && probed == 1
                            && matches == 1 && (delayed == 0 || live == 1)
                            && (owned == 1 || (batch == 1 && exact_run == 1
                                && tail_fresh == 1 && prior_blank == 1)) { 1 } else { 0 }
                    };
            }

            invariant ExactEchoAdmitted:
                if phase == 3 && first == 1 && printed == 1 && probed == 1
                    && owned == 1 && matches == 1
                    && (delayed == 0 || live == 1) { admitted == 1 }
                else { admitted <= 1 };
            invariant ExactBatchAdmitted:
                if phase == 3 && first == 1 && printed == 1 && probed == 1
                    && batch == 1 && exact_run == 1 && tail_fresh == 1
                    && prior_blank == 1 && matches == 1 { admitted == 1 }
                else { admitted <= 1 };
            invariant NoKeylessClaim: admitted <= first;
            invariant NoStalePrintClaim: admitted <= printed;
            invariant NoStaleProbeClaim: admitted <= probed;
            invariant NoUnownedClaim: admitted <= owned + batch;
            invariant NoOtherGlyphClaim: admitted <= matches;
            invariant NoExpiredDelayedClaim:
                if delayed == 1 && live == 0 { admitted == 0 }
                else { admitted <= 1 };
            invariant NoUnprovenBatch:
                if batch == 1 { admitted <= exact_run && admitted <= tail_fresh
                    && admitted <= prior_blank } else { admitted <= 1 };
            invariant OneCreditPerAdmission: spent == admitted;
            invariant Bounded:
                phase <= 3 && first <= 1 && second <= 1 && printed <= 1
                    && probed <= 1 && owned <= 1 && batch <= 1
                    && exact_run <= 1 && tail_fresh <= 1 && prior_blank <= 1
                    && matches <= 1 && delayed <= 1 && live <= 1
                    && admitted <= 1 && spent <= 1;
        }
    }
}

/// An unknown-width insert can absorb a queued key into its own hop, so its
/// dispatch-to-delivery credits leave the generic ring at orphan cleanup.
/// When exactly one known one-cell key remains, the seam escrows its glyph
/// and the insert's row/next column/print generation. Only that exact later
/// row transition can spend it; a different print, another pending key,
/// expiry, scroll, rewrite, or reset closes the one-shot escrow. Tier-1 drives
/// the genuine CursorGlow seam in `cursor_glow::tests`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn unknown_insert_orphan_key_model() -> Model {
    crate::ty_model! {
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
                queued = 3; generic = 1; phase = 1;
            }
            action QueueTwo when (phase == 0) {
                queued = 2; generic = 2; phase = 1;
            }
            action QueueNone when (phase == 0) { phase = 1; }
            action LayWithSite when (phase == 1) { site = 1; phase = 2; }
            action LayWithoutSite when (phase == 1) { phase = 2; }
            action Cleanup when (phase == 2) {
                generic = if Buggy == 1 { generic } else { 0 };
                escrow = if queued == 1 && site == 1 { 1 } else { 0 };
                phase = 3;
            }
            action ExactNewPrint when (phase == 3) {
                exact = 1;
                lit = if Buggy == 1 || (escrow == 1 && site == 1) { 1 } else { 0 };
                escrow = if Buggy == 1 { escrow } else { 0 };
                phase = 4;
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
            action LaterProgramPrint when (phase == 4) { phase = 5; }

            invariant NoGenericAfterCleanup:
                if phase > 2 { generic == 0 } else { generic <= 2 };
            invariant ExactOnly: lit <= exact;
            invariant SiteRequired: lit <= site;
            invariant OneShot: lit + escrow <= 1;
            invariant Bounded:
                phase <= 5 && queued <= 3 && generic <= 2 && site <= 1
                    && escrow <= 1 && exact <= 1 && lit <= 1;
        }
    }
}
