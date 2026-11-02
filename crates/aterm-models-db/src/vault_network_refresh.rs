// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Closed refresh outcomes are context bound and independent of usable local cache.
/// Time tokens distinguish the prior result, a later clock reading and clock rollback.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vault_network_refresh_model() -> Model {
    aterm_spec::ty_model! {
        DbVaultNetworkRefresh {
            const Buggy = 0;
            var case = 0; var phase = 0;
            var recorded = 0; var outcome = 1; var stamp = 1;
            var shown = 0; var shown_outcome = 0; var shown_stamp = 0;
            var preserved = 1;
            action Case when (phase == 0 && 18 > case) { case = case + 1; }
            action Complete when (phase == 0) {
                phase = 1;
                recorded = if (Buggy == 1 && case == 2) || (Buggy == 2 && case == 3)
                    || (Buggy == 3 && case == 7) || (Buggy == 11 && case == 15) {1}
                    else if Buggy == 6 && case == 12 {0}
                    else if case <= 1 || (4 <= case && case <= 6)
                        || (10 <= case && case <= 13) || 16 <= case {1} else {0};
                outcome = if (Buggy == 1 && case == 2) || (Buggy == 2 && case == 3)
                    || (Buggy == 11 && case == 15) || (Buggy == 14 && case == 18) {2}
                    else if (Buggy == 5 && case == 1) || (Buggy == 6 && case == 12) {1}
                    else if case == 1 || case == 6 || case == 12 || case == 16 {2} else {1};
                stamp = if (Buggy == 1 && case == 2) || (Buggy == 2 && case == 3)
                    || (Buggy == 3 && case == 7) || (Buggy == 11 && case == 15) {2}
                    else if Buggy == 6 && case == 12 {1}
                    else if case == 12 || case == 17 {3}
                    else if case <= 1 || (4 <= case && case <= 6)
                        || (10 <= case && case <= 13) || 16 <= case {2} else {1};
                preserved = if Buggy == 8 {0} else {1};
            }
            action Project when (phase == 1) {
                phase = 2;
                shown = if (Buggy == 4 && case == 11) || (Buggy == 12 && case == 10) {1}
                    else if Buggy == 10 && case == 0 {0}
                    else if 7 <= case && case <= 11 {0} else {1};
                shown_outcome = if Buggy == 13 && case == 0 {2}
                    else if (Buggy == 4 && case == 11) || (Buggy == 12 && case == 10) {outcome}
                    else if (Buggy == 10 && case == 0) || (7 <= case && case <= 11) {0}
                    else {outcome};
                shown_stamp = if (Buggy == 4 && case == 11) || (Buggy == 12 && case == 10) {stamp}
                    else if (Buggy == 10 && case == 0) || (7 <= case && case <= 11) {0}
                    else if Buggy == 7 {4} else {stamp};
            }
            action Reload when (phase == 2) {
                phase = 3;
                stamp = if Buggy == 9 {4} else {stamp};
                shown_stamp = if Buggy == 9 && shown == 1 {4} else {shown_stamp};
            }
            invariant CompletionAdmission: phase == 0 || recorded ==
                if case <= 1 || (4 <= case && case <= 6)
                    || (10 <= case && case <= 13) || 16 <= case {1} else {0};
            invariant LatestOutcome: phase == 0 || outcome ==
                if case == 1 || case == 6 || case == 12 || case == 16 {2} else {1};
            invariant CompletionOrder: phase == 0 || stamp ==
                if case == 12 || case == 17 {3}
                else if case <= 1 || (4 <= case && case <= 6)
                    || (10 <= case && case <= 13) || 16 <= case {2} else {1};
            invariant ContextProjection: phase <= 1 || shown ==
                if 7 <= case && case <= 11 {0} else {1};
            invariant VisibleClosedOutcome: phase <= 1 || shown_outcome ==
                if shown == 1 {outcome} else {0};
            invariant OriginalCompletionTime: phase <= 1 || shown_stamp ==
                if shown == 1 {stamp} else {0};
            invariant AuthorityPreserved: preserved == 1;
            bound PhaseRange: phase <= 3;
            bound CaseRange: case <= 18;
        }
    }
}
