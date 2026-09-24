// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A retired typed run can rejoin an adjacent key only when the current row
/// still spells its exact cached glyphs, the hand stayed on that row, and the
/// echo is typed and inside the five-second chain window. The cache is
/// dormant: idle ticks neither scan it nor schedule a frame. `Buggy=1` loses
/// the owner's `and ` prefix by treating a licence clear as content erasure;
/// `Buggy=2` revives it on keyless
/// program paint; `Buggy=3` wakes the idle renderer for dormant metadata.
///
/// Tier-1 in `aterm-effects/tests/codex_particle_replay.rs` replays Codex
/// 0.155.1's captured wrapped composer at the measured five-hertz cadence,
/// projecting the separate-turn licence clear, exact-content resume, and its
/// changed, keyless, navigation, and stale controls onto the real ribbon's
/// newly born prefix cells.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_typed_continuity_model() -> Model {
    crate::ty_model! {
        RainbowTypedContinuity {
            const Buggy = 0;
            var phase = 0;
            var authored = 0;
            var cached = 0;
            var live = 0;
            var licence = 0;
            var cleared = 0;
            var same_row = 1;
            var adjacent = 1;
            var exact = 1;
            var fresh = 1;
            var typed = 0;
            var revived = 0;
            var idle_work = 0;

            action TypeRun when (phase == 0) {
                phase = 1; authored = 1; cached = 1; live = 1; licence = 1;
            }
            action RetireNaturally when (phase == 1) {
                phase = 2; live = 0;
            }
            action ClearLicence when (phase == 2) {
                licence = 0; cleared = 1;
                cached = if Buggy == 1 { 0 } else { cached };
            }
            action IdleTick when (phase == 2) {
                idle_work = if Buggy == 1 || Buggy == 3 { 1 } else { 0 };
            }
            action ChangeGlyph when (phase == 2) { exact = 0; }
            action MoveRow when (phase == 2) { same_row = 0; }
            action LeaveAdjacentEnd when (phase == 2) { adjacent = 0; }
            action Navigation when (phase == 2) { cached = 0; authored = 0; }
            action AgePastChain when (phase == 2) { fresh = 0; }
            action ResumeTyped when (phase == 2) {
                phase = 3; typed = 1; licence = 1;
                revived = if Buggy == 1 {
                    if cleared == 1 && cached == 0 && same_row == 1
                        && adjacent == 1 && exact == 1 && fresh == 1 { 0 }
                    else { 1 }
                } else {
                    if cached == 1 && live == 0 && same_row == 1
                        && adjacent == 1 && exact == 1 && fresh == 1 { 1 }
                    else { 0 }
                };
            }
            action ProgramPaint when (phase == 2) {
                phase = 3; typed = 0;
                revived = if Buggy == 1 || Buggy == 2 { 1 } else { 0 };
            }

            invariant ExactTypedContinuationRejoins:
                if phase == 3 && typed == 1 && authored == 1 && live == 0
                    && same_row == 1 && adjacent == 1 && exact == 1 && fresh == 1 {
                    revived == 1
                } else { revived <= 1 };
            invariant LicenceClearPreservesContent:
                if phase == 2 && cleared == 1 && authored == 1 { cached == 1 }
                else { cached <= 1 };
            invariant RejoinNeedsTypedEcho: revived <= typed;
            invariant RejoinNeedsCachedRun: revived <= cached;
            invariant RejoinNeedsSameRow: revived <= same_row;
            invariant RejoinNeedsAdjacency: revived <= adjacent;
            invariant RejoinNeedsExactContent: revived <= exact;
            invariant RejoinNeedsFreshChain: revived <= fresh;
            invariant DormantCacheDoesNotWake: idle_work == 0;
            invariant Bounded:
                phase <= 3 && authored <= 1 && cached <= 1 && live <= 1
                    && licence <= 1 && cleared <= 1 && same_row <= 1
                    && adjacent <= 1 && exact <= 1 && fresh <= 1
                    && typed <= 1 && revived <= 1 && idle_work <= 1;
        }
    }
}
