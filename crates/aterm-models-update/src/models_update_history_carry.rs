// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's history carry: every history line the outgoing process held
//! crosses to the successor or is counted; none is dropped silently, replaced by lines that
//! are no longer the session's history, or put back after the successor's pane cleared its
//! scrollback.
//!
//! The export takes the whole history, fenced, with readers live; the park takes the
//! screen checkpoint plus the fence as it then stands; the worker joins the two (the
//! sidecar's older lines cross when the fence still holds and the checkpoint reaches back
//! to the export's end); the successor imports the sidecar after Commit.

use aterm_spec::derive::Model;

/// One session's history across one handoff.
///
/// State: `phase` — 0 live, 1 exported, 2 parked (the join named the sidecar or counted
/// the fallback), 3 settled (the successor imported after Commit). `exp` — lines the export
/// fenced; `late` — lines output between export and park; `moved` — the history was
/// rewrapped, cleared or reset after the export (its fence no longer holds); `total` — the
/// history the park saw; `carried` — lines the screen checkpoint carries; `take` — sidecar
/// lines the manifest names; `dropped` — lines the outgoing side counted as not crossing;
/// `intact` — the sidecar matches its stamped length and sha; `cleared` — the successor's
/// pane cleared its scrollback (ED3, a reset) before the import; `held` — lines the
/// successor holds once settled; `failed` — lines it counted when the sidecar failed.
///
/// `Cap` stands for the screen carry's bound (`seamless::MAX_HANDOFF_HISTORY_LINES`, 256 in
/// the product) and `Deep` for a history deeper than it; the shallow case is the join's
/// `take == 0` arm, bound at Tier-1 on the real `join`.
///
/// `Buggy = 1` is four defects, each caught alone: a fallback that drops the export and
/// counts nothing, a join that names the sidecar though the history moved under it, a
/// successor that drops a sidecar failing its sha without counting it, and an import that
/// puts history back after the pane cleared it.
///
/// Tier-1: `aterm-gui/src/handoff_history_conformance.rs` drives the real export, park,
/// join, sidecar and import over every path, with `Buggy = 1` as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_history_carry_model() -> Model {
    aterm_spec::ty_model! {
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
