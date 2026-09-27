// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's HISTORY CARRY (2026-09-26): every history line
//! the outgoing process held crosses to the successor, or is COUNTED — never
//! dropped in silence, never replaced by lines that were not the session's
//! history any more, and never put back after the successor's pane CLEARED
//! its scrollback.
//!
//! Until this date an in-session update carried at most 256 lines of each
//! tab's scrollback inside the screen checkpoint (fewer, or none, under
//! deadline pressure) and the rest stayed behind with one line in aterm.log to
//! say so. The carry that replaced it exports the whole history with every
//! reader live, fenced; the park takes the screen exactly as before plus the
//! fence as it then stands; the worker JOINS the two — the sidecar's lines
//! older than the checkpoint's cross when the fence still holds and the
//! checkpoint reaches back to the export's end — and the successor imports
//! the sidecar after Commit.

use super::Model;

/// One session's history across one handoff.
///
/// State: `phase` — 0 live (readers running), 1 exported, 2 parked (the join
/// has named the sidecar or counted the fallback), 3 settled (the successor
/// imported after Commit). `exp` — lines the export fenced; `late` — lines
/// output between the export and the park; `moved` — the history was
/// rewrapped, cleared or reset after the export (its fence no longer holds);
/// `total` — the history the park saw; `carried` — the lines the screen
/// checkpoint carries; `take` — sidecar lines the manifest record names;
/// `dropped` — lines the outgoing side counted as not crossing; `intact` —
/// the sidecar arrives with its stamped length and sha; `cleared` — the
/// successor's pane cleared its scrollback (ED3, a reset) after the adopt and
/// before the import; `held` — history lines the successor holds once
/// settled; `failed` — lines the successor counted when the sidecar failed
/// its checks.
///
/// Constants: `Cap` is the screen carry's bound (256 in the product) and
/// `Deep` a history deeper than it — the case the carry exists for; the
/// shallow case (everything fits the checkpoint) is the join's `take == 0`
/// arm, bound at Tier-1 on the real `join`.
///
/// Invariants: `NothingSilentlyLost` — settled, the successor's lines plus
/// every line counted on either side are exactly the history the park saw
/// (unless the successor's pane cleared it, which is not a loss).
/// `SidecarOnlyOverItsOwnHistory` — a sidecar is named only while its fence
/// holds and it meets the checkpoint's lines, so what the successor imports is
/// the session's history, contiguous, and nothing else.
/// `ClearedStaysCleared` — a pane that cleared its scrollback before the
/// import holds none of it once settled.
///
/// `Buggy = 1` is the four ways this goes wrong, each caught alone: a
/// fallback that drops the export and counts nothing (the defect the carry was
/// written for — scrollback that is simply gone after the update, with no
/// word), a join that names the sidecar though the history moved under it
/// (a cleared history resurrected, a rewrapped one imported at the wrong
/// width), a successor that drops a sidecar failing its sha without counting
/// it, and an import that lands after the successor's pane cleared its
/// scrollback and puts the history back (the 2026-09-26 review measured all
/// 46 cleared lines back).
///
/// Tier-1 (`aterm-gui/src/handoff_history_conformance.rs`) drives the real
/// export, park, join, sidecar, take-before-proof and import over every path
/// of this machine on real engines, projects `exp`, `total`, `carried`,
/// `take`, `dropped`, `held` and `failed` at every step, and replays the
/// `Buggy = 1` machine's paths as negative controls the real code refuses.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_history_carry_model() -> Model {
    crate::ty_model! {
        NativeUpdateHistoryCarry {
            const Buggy = 0;
            const Cap = 1;
            const Deep = 2;
            var phase = 0;
            var exp = 0;
            var late = 0;
            var moved = 0;
            var total = 0;
            var carried = 0;
            var take = 0;
            var dropped = 0;
            var intact = 1;
            var cleared = 0;
            var held = 0;
            var failed = 0;

            action Export when (phase == 0) {
                phase = 1;
                exp = Deep;
            }
            action Output when (phase == 1 && late <= 1) {
                late = late + 1;
            }
            action Rewrap when (phase == 1 && moved == 0) {
                moved = 1;
            }
            action Park when (phase == 1) {
                phase = 2;
                total = exp + late;
                carried = Cap;
                take = if (moved == 0 || Buggy == 1) && late <= Cap {
                    exp + late - Cap
                } else {
                    0
                };
                dropped = if (moved == 0 || Buggy == 1) && late <= Cap {
                    0
                } else if Buggy == 1 {
                    0
                } else {
                    exp + late - Cap
                };
            }
            action Corrupt when (phase == 2 && intact == 1 && take > 0 && cleared == 0) {
                intact = 0;
            }
            action Clear when (phase == 2 && cleared == 0) {
                cleared = 1;
            }
            action Settle when (phase == 2) {
                phase = 3;
                held = if cleared == 1 && Buggy == 1 && intact == 1 {
                    take
                } else if cleared == 1 {
                    0
                } else if intact == 1 {
                    carried + take
                } else {
                    carried
                };
                failed = if cleared == 1 {
                    0
                } else if intact == 1 {
                    0
                } else if Buggy == 1 {
                    0
                } else {
                    take
                };
            }
            action Settled when (phase == 3) {
                phase = 3;
            }

            invariant NothingSilentlyLost:
                if phase == 3 {
                    cleared == 1 || held + dropped + failed == total
                } else {
                    held == 0
                };
            invariant SidecarOnlyOverItsOwnHistory:
                if take > 0 { moved == 0 && late <= carried } else { take == 0 };
            invariant ClearedStaysCleared:
                if phase == 3 && cleared == 1 { held == 0 } else { cleared <= 1 };
        }
    }
}
